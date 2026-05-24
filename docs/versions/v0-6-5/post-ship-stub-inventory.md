# V0.6.5 Post-ship Stub Inventory

> **Purpose:** SoT 罗列 V0.6.5 ship 后 codebase 中仍存在的 stub / mock / placeholder / "未实现" 路径,为 V0.6.6 / V0.7 finding 候选提供输入。
> **Scope:** read-only audit;**不修代码**。
> **Last scan:** 2026-05-24, HEAD = `fd1d946 v0.6.5: integrate new capabilities into user-facing docs (ship gate) (#118)`
> **Baseline:** `cargo test --workspace --locked --no-fail-fast` = 1583 / 1 · `cargo clippy --workspace --all-targets --locked -- -D warnings` = 0 warning.
> **Methodology:** ripgrep with patterns `todo!()` / `unimplemented!()` / `panic!("not implemented")` / `// TODO` / `// FIXME` / `// HACK` / 占位 / 未实现 / mock / placeholder / `Err("not implemented")` / `V0.[6-9]` future-version refs.
>   - **Excludes** test sources (`crates/*/tests/**` + `**/tests.rs`) for production-side scans — test-only stubs are listed separately.
>   - **Excludes** intentional template / fixture text (e.g. `handoff.rs::HANDOFF_TEMPLATE` content, host-probe stub fixtures in `scripts/host-probe/`, `claude-mem` placeholder template engine semantics).

---

## Summary

| Category | Count | Suggested target |
|---|---|---|
| 1. Hard stub (`todo!()` / `unimplemented!()` / `panic!("not implemented")`) | 0 production · 1 build-script (intentional fallback) | n/a |
| 2. Production paths returning `Err(NotImplemented{..})` by design | 2 (Claude `claude_bg::resume_thread` red-line R3; `claude_tui::resume_thread` falls back to recovery) | n/a — both are documented contract (CLAUDE.md §三 R3) |
| 3. Real "WIP returning false / stub data" production functions | 1 — `verify_slack_signature_stub` (unused in prod;占位 for V0.7 Slack inbound) | V0.7 (already on `dev-coupling-audit.md` deferred list) |
| 4. Future-version-marker comments in Rust src (`// TODO` / `V0.7+` references) | 11 production sites | mostly V0.7 (cluster); 1 V0.6.6 candidate (F156-spinoff cost rollup) |
| 5. Cargo feature flags reserved for future providers (China-platform) | 4 features (`lark`/`dingtalk`/`qq`/`wechat`) flagged in `Cargo.toml` comment as "placeholder feature names — provider modules land in V0.7" | V0.7 Epic C |
| 6. Adapter delegation today / Wave-3 swap planned | 3 sites — `default_adapter_factory` Codex arm → `ClaudeTuiAdapter` fallback; `adapter_for_chat` Codex/Chat arm; `HumanApprovalAdapter` wrapper not yet introduced | V0.7 |
| 7. Stale doc-comment references to retired/landed work | 4 (one each in `dashboard.rs` / `team.rs` / `harness.rs` / `claude_bg.rs::events` doc) | V0.6.6 doc-only chore |
| 8. Skill/template/script "stub" hits | 0 real placeholders — all hits are intentional fixtures or descriptive text | n/a (F159/F161 already cleared) |
| 9. Test-only stub adapters (`RecordingAdapter` etc.) | 8 sites returning `NotImplemented{reason:"stub"}` in `#[cfg(test)]` mod tests / `tests/*.rs` | n/a (test fixtures, do not change) |

---

## Detailed Inventory

### Category 1: Hard stub (`todo!()` / `unimplemented!()` / `panic!`)

| Location | Pattern | Description | Suggested target | Reason deferred |
|---|---|---|---|---|
| `crates/ccteam-web/build.rs:138` | `panic!("write placeholder index.html: {err}")` | Build-script fallback panic when writing the placeholder `index.html` for `--no-default-features` builds. Not a runtime stub — only fires if the placeholder file write itself fails. | n/a | Intentional build-time hard-fail. **Skip.** |

**Production code is `todo!()`-free and `unimplemented!()`-free.**

### Category 2: Production paths returning `Err(NotImplemented)` by design

| Location | Pattern | Description | Suggested target | Reason deferred |
|---|---|---|---|---|
| `crates/ccteam-core/src/execution/claude_bg.rs:149` | `Err(HarnessError::NotImplemented{reason: "claude --bg is single-turn fresh-context every spawn (red line R3 …"})` | Bg adapter's `resume_thread` intentionally refuses — CLAUDE.md §三 "每次 spawn = fresh 1M context (bg 模式)" red line. | n/a — red line | Permanent contract. **Skip.** |
| `crates/ccteam-core/src/execution/claude_tui.rs:423` | `Err(HarnessError::NotImplemented{reason: "resume_thread requires a live tmux session …"})` | Chat adapter's `resume_thread` errors when the named tmux session is absent, instructing caller to fall back to `start_thread` + `session_recovery::build_recovery_prompt`. | n/a — production caller contract | Not a stub — documented behaviour. **Skip.** |

### Category 3: Real WIP "returns false / stub data" production functions

| Location | Pattern | Description | Suggested target | Reason deferred |
|---|---|---|---|---|
| `crates/ccteam-imd/src/three_layer_sec.rs:91-114` `verify_slack_signature_stub` | Function body: replay-window check OK, then unconditionally returns `false` ("No HMAC backend wired yet; conservative deny by default. Slack inbound HTTP receiver is V0.7 scope"). | Slack `v0:<ts>:<body>` HMAC-SHA256 sig verification is stubbed. Only call site is its own unit test — **no production caller**, since V0.6 Slack uses polling (`SlackChannel` in `transport/providers/slack.rs`) which carries no signed-request requirement. | **V0.7** (Epic C — Slack Socket Mode / inbound HTTP) | Already deferred in `transport/providers/slack.rs:5` ("Switch to Socket Mode is a V0.7 decision per `docs/versions/v0-6-0/wave-2-decisions.md` §5"). Adds `hmac` / `sha2` / `subtle` deps. |

### Category 4: Future-version-marker comments in Rust src (production-side)

| Location | Note | Suggested target |
|---|---|---|
| `crates/ccteam-imd/src/daemon.rs:84` | `// TODO(wave-3 codex-exec-impl): swap to CodexAppServerAdapter once it lands.` Inside `default_adapter_factory`: Codex arm currently returns `ClaudeTuiAdapter` instead of the real `CodexAppServerAdapter`. **Real wiring gap** — Codex chat-mode bots silently get a Claude adapter today. | V0.7 (F156-class — daemon-routed Codex adoption; consider V0.6.6 if a user actually hits this path) |
| `crates/ccteam-imd/src/daemon.rs:409` | `/// 3. (slack / discord — TODO in V0.7: providers exist but the host probe's first round only exercises telegram).` `build_channels` only wires the telegram channel; slack/discord crates exist but daemon doesn't construct them. | V0.7 |
| `crates/ccteam-imd/src/daemon.rs:458` | `// (single-bot host probe), V0.7 will cache.` `list_bots()` called per inbound message; small in V0.6.x, will need caching at scale. | V0.7 perf-rework |
| `crates/ccteam-imd/src/daemon.rs:559-561` | "V0.7 will land per-bot custom handles. For F132 the typical … collision is theoretical until V0.7." `build_handle_map` last-wins on duplicate role across slugs. | V0.7 Epic C (workflow.yaml `chat_handle:` per-bot) |
| `crates/ccteam-imd/src/nl_admin.rs:271` | `// V0.7 wires the full ccteam_cost rollup here.` `cost_today` returns registry-count summary; not real per-vendor 24h aggregation. | V0.6.6 (cheap follow-up; `ccteam-cost` ledger from F152 makes this small) |
| `crates/ccteam-imd/src/three_layer_sec.rs:111` | `// Slack inbound HTTP receiver is V0.7 scope` — covered in Category 3. | V0.7 |
| `crates/ccteam-imd/src/transport/providers/slack.rs:5` | `// Switch to Socket Mode is a V0.7 decision` — design note. | V0.7 |
| `crates/ccteam-cost/src/budget.rs:7` | `// Wave 2 / V0.7 deprecates the flat path.` Legacy V0.5 `BudgetSpec` still parallel to per-vendor `Budgets`. | V0.7 (breaking rename, pre-v1.0 allowed) |
| `crates/ccteam-core/src/workflow.rs:82` | `// `None` keeps the V0.5 flat-budget semantics (Wave 2 / V0.7 deprecates BudgetSpec).` Same root cause as previous. | V0.7 (paired with `cost/budget.rs:7`) |
| `crates/ccteam-cli/src/commands.rs:4043` | `// fallback section has user-visible keys; V0.7+ can add more by extending the parse_key match arm` — extensibility hook, no current stub. | V0.7+ (additive) |
| `crates/ccteam-cli/src/main.rs:627` | `// (off | codex); V0.7+ will fold in additional opt-in preferences.` Same — extensibility marker. | V0.7+ (additive) |
| `crates/ccteam-core/src/templates/workflow_templates/mod.rs:19` | `// necessary in V0.7+, swap to handlebars at that point.` Plain `{{var}}` replacement may need template-engine upgrade. | V0.7+ (only if presets gain conditionals) |
| `crates/ccteam-core/src/preferences.rs:8,43` | `// (vendor fallback today; more knobs in V0.7+)` / `// Adding a new section in V0.7+` — extensibility hooks. | V0.7+ (additive) |
| `crates/ccteam-core/src/orchestrator.rs:672-689` | `// CodexAppServerAdapter ships with Wave 3 codex-exec-impl … until that lands, fall back to the bg adapter` (Codex+Chat dispatch table) and `TODO(F124 full scope, post-F98): introduce a dedicated HumanApprovalAdapter wrapper`. | V0.7 (paired with `daemon.rs:84` Codex chat path; F124 full-scope HumanApprovalAdapter) |
| `crates/ccteam-core/src/harness.rs:308` | `/// Wave 2 / Wave 3 adapters will populate this stream` — trait-level doc note that some adapters return `stream::empty()`. | V0.7 (concrete impact: `ClaudeBgAdapter::events` empty stream — see Category 7) |
| `crates/ccteam-core/src/execution/claude_bg.rs:140-146` | `// Wave 1: empty stream. … Wave 2 / Wave 3 will populate this stream and the poller will be retired.` `events()` returns `stream::empty()`; orchestrator's F80 stale-spawn poller still drives `agent_done`. | V0.7 (orchestrator architecture work — couples with §dev-coupling-audit REQ-004 file-IPC → Event Bus) |
| `crates/ccteam-core/src/execution/codex_app_server.rs:20` | `// (the V0.6.0 Wave 3 D9 retained …)` historic Wave 3 reference. | n/a (historical) |
| `crates/ccteam-imd/src/supervisor.rs:848` | `// (V0.6 Wave 3: enforcement landed in handle_inbound is intentionally minimal — Wave 4 policy hook decides the drain UX.)` `SupervisorAction::Drain` flips `draining=true` but no full policy hook. | V0.7 (drain-policy UX) |

### Category 5: Cargo feature flags reserved for future providers

| Location | Description | Suggested target |
|---|---|---|
| `crates/ccteam-imd/Cargo.toml:50-70` | Cargo features `lark`, `dingtalk`, `qq`, `wechat` declared as empty `[]` with comment: `# China-platform names (lark/dingtalk/qq/wechat) are placeholder feature names — provider modules land in V0.7.` Also `signal`, `imessage`, `matrix`, `whatsapp` declared but with no provider source files yet. | V0.7 Epic C (国内 IM) |

### Category 6: Adapter delegation / Wave-3 swap planned

| Location | Description | Suggested target |
|---|---|---|
| `crates/ccteam-imd/src/daemon.rs:78-89` `default_adapter_factory` | Codex bots silently get `ClaudeTuiAdapter` (with comment "until [`CodexAppServerAdapter`] lands"). Misregistered Codex bot → `start_thread` will fail noisily on the wrong vendor — not user-facing broken, but **a real functional gap** for V0.7 chat-mode Codex onboarding. | V0.7 (couples with F156-derived "daemon-routed Codex critic" defer) |
| `crates/ccteam-core/src/orchestrator.rs:671-677` `adapter_for_chat` | `(Codex, Chat)` arm falls back to bg adapter "so the dispatch table never returns None"; comment says CodexAppServerAdapter ships with Wave 3 codex-exec-impl. | Same as above — joint V0.7 |
| `crates/ccteam-core/src/orchestrator.rs:684-689` HumanApproval arm | `TODO(F124 full scope, post-F98): introduce a dedicated HumanApprovalAdapter wrapper that delegates spawn to the inner adapter but tags spawn metadata for the IM round-trip + plan_decision injection.` Currently just delegates to `adapter_for(exec)`. | V0.7 (F124 full-scope expansion) |

### Category 7: Stale doc-comment references to retired / landed work

| Location | Description | Suggested target |
|---|---|---|
| `crates/ccteam-web/src/routes/dashboard.rs:10` | `// see the TODO marker there for V0.3.3 cleanup.` Points to `assets.rs` for a TODO that no longer exists (already done). | V0.6.6 doc-only chore |
| `crates/ccteam-core/src/team.rs:1503` | `// F47 ship: schema accepts harness: codex even though spawn is still NotImplemented. F49 wires the runtime path.` Stale — F49 has long shipped. | V0.6.6 doc-only chore |
| `crates/ccteam-cost/src/pricing.rs:51` | `// `ccteam-core` will re-export `ccteam_cost::Vendor` once Wave 2 lands.` Wave 2 of V0.6.0 long-shipped; verify re-export state. | V0.6.6 doc-only chore |
| `crates/ccteam-core/src/templates/project_mcp_json.rs:18` | `// Wave 1 lands this helper alone; Wave 2 wires it into the ccteam-creator skill execute phase` — confirm wiring already exists (creator phase 5 in F148 should call it). | V0.6.6 doc-only chore (verify + scrub) |
| `crates/ccteam-imd/src/transport/mod.rs:3` | `// V0.6.0 Wave 2 Option-C implementation` — historical reference, ok to keep but consider trimming. | n/a (keep — historical doc) |

### Category 8: Skill / template / script body placeholder

**No real placeholder bodies found.** All hits in skills/scripts are:
- intentional descriptive text (e.g. `ccteam-scan/SKILL.md` `Q2 — TODO / FIXME / HACK 热点` is part of the audit checklist, **not** a stub),
- F159 regression-guard tooling (`skills/ccteam/SKILL.md` lines 103-114 + `crates/ccteam-core/tests/dispatcher_hide_unimpl_test.rs`),
- fixture stubs in `scripts/host-probe/run-probes.sh` (intentional fake-claude binary for probe scenarios),
- F162 mock-mode classifier (`scripts/host-probe/intent-accuracy.sh`).

F149 / F154 / F159 / F161 already scrubbed all "Wave N / STUB / NotImplemented / 占位 / 准备中 / 待落地" from `skills/*/SKILL.md` and tier-1 user-facing docs.

### Category 9: Test-only stub adapters

These are `#[cfg(test)]` or `tests/*.rs` adapter implementations (`RecordingAdapter`, `MockAdapter`, etc.) that return `Err(HarnessError::NotImplemented{reason:"stub"})` for unused methods (typically `resume_thread`):

- `crates/ccteam-imd/src/supervisor.rs:927`
- `crates/ccteam-imd/src/daemon.rs:1289`
- `crates/ccteam-imd/tests/e2e_mock_test.rs:131,306`
- `crates/ccteam-imd/tests/heartbeat_writer_test.rs:67`
- `crates/ccteam-imd/tests/dm_autoroute_test.rs:189`
- `crates/ccteam-imd/tests/outbound_wiring_test.rs:94`
- `crates/ccteam-imd/tests/inbound_wiring_test.rs:101`
- `crates/ccteam-imd/tests/turns_mirror_consumer_test.rs:86`
- `crates/ccteam-imd/tests/chat_reset_signal_test.rs:71`
- `crates/ccteam-imd/tests/chat_send_input_test.rs:71`

**Action:** none. These are correct test fixtures. Listing for completeness.

---

## Cross-reference with explicit-defer findings (V0.6.5 + earlier)

From `docs/versions/v0-6-5/wave-{1,2,3}-handoff.md` + `docs/dev-coupling-audit.md`:

- **F156 daemon-routed Codex critic with unified cost accounting** — explicit V0.7+ defer (Wave 2 handoff R8 + Rejected list). Couples with `daemon.rs:84` + `orchestrator.rs:672` Codex adapter swap.
- **F148 / F157 / F162 / F163 / F164 nas-box005 real host-probe** — not a stub, deferred to host-probe sign-off (Wave 4b, separately tracked).
- **chat MCP multi-bot per `chat_id`** — V0.7 candidate (Wave 1 Rejected).
- **`F157 wall-clock ≤90 s` real-repo probe** — Wave 4 deferred (organic, Wave 3 Remaining).
- **`F162 --real` LLM-mode run** — Wave 4 deferred (organic, Wave 3 Remaining).
- **Audit team REQ findings** (per CLAUDE.md §四 / `docs/versions/v0-6-5/README.md` §4):
  - REQ-002 `ProjectState` god-object → V0.7.x patch
  - REQ-004 file-IPC → Event Bus → V0.7.x or independent minor (couples with `claude_bg::events` empty stream + `harness.rs:308`)
  - REQ-006 `ccteam-core` monolith split → V0.7.x patch
- **`monorepo-aware .mcp.json`** — V0.7+ (researcher R6#4)
- **`ccteam migrate-from-claude`** — V0.7+ (codex-expert CX6#5)
- **DM cross-device sync (chat memory)** — V0.7+
- **non-Anthropic/non-OpenAI vendor (Gemini / DeepSeek / Qwen)** — V0.7+

---

## Recommendations

### V0.6.6 patch 候选 (small chore PRs)

1. **`nl_admin::cost_today` ccteam_cost rollup wire-up** (`crates/ccteam-imd/src/nl_admin.rs:271`). V0.6.5 F152 shipped `<ccteam_root>/cost-budget.json` ledger; wiring `cost_today` to read it is mechanical (~50 LOC + 2 tests). Closes a user-visible "ccteam cost today" IM admin response that currently returns only a bot count.
2. **Stale doc-comment scrub** (Category 7, ~5 sites). Pure doc chore: `dashboard.rs:10` / `team.rs:1503` / `pricing.rs:51` / `project_mcp_json.rs:18` / `claude_bg.rs::events` doc (the empty-stream comment can stay until Event-Bus minor lands, but rewording to remove "Wave 2 / Wave 3 will" promise is cheap). Zero code-behaviour change, blocks documentation drift accumulating into V0.7.
3. **`ccteam doctor` stub-counter parity** — V0.6.5 ship gate item #9 says `cargo run --release -- doctor` should output `"MCP tool surface: 26 active, 0 stubs"`. Verify the doctor output strings still say "0 stubs" post-Wave 4 (no regression), and consider adding a small "production-side stubs" counter that surfaces `verify_slack_signature_stub` + `daemon.rs:84` Codex adapter fallback — gives users a visible deferred-feature signal.

### V0.7 minor 主候选 (architecture / new capability)

1. **Daemon-routed Codex adoption (chat mode)** — couples F156 partial defer + `daemon.rs:84` + `orchestrator.rs:672`. Real `CodexAppServerAdapter` registered in `default_adapter_factory` Codex arm; unblocks Codex chat-mode bots end-to-end. Pairs with unified cost-rollup ledger.
2. **Slack/Discord daemon channel wiring + Slack HMAC** — couples `daemon.rs:409` `build_channels` slack/discord branch + `three_layer_sec::verify_slack_signature_stub` real impl (hmac/sha2/subtle deps). Unblocks `ccteam-im-setup` skill's currently-rejected Slack/Discord onboarding paths.
3. **China-platform IM providers** (Epic C — `lark` / `dingtalk` / `qq` / `wechat`) — Cargo features already reserved; provider source files + `Channel` impls + credentials wiring. Couples with `ccteam-im-setup` skill UX.
4. **`HumanApprovalAdapter` wrapper** (F124 full scope) — `orchestrator.rs:684` TODO. Dedicated wrapper tagging spawn metadata for IM round-trip + plan_decision injection.
5. **Per-bot `chat_handle:` in workflow.yaml** — `daemon.rs:559` collision risk becomes real once a single deployment runs the same `role` across slugs (e.g. two `tg-helper` bots).

### V0.8+ 主线 (large architecture)

- **File-IPC → Event Bus** (audit REQ-004) — couples with `claude_bg::events` populating real stream + retiring F80 stale-spawn poller. Touches every adapter + orchestrator.
- **`ccteam-core` 巨石拆分** (audit REQ-006).
- **`ProjectState` god-object decomposition** (audit REQ-002).
- **`BudgetSpec` → `Budgets` deprecation** (`cost/budget.rs:7` + `workflow.rs:82`) — breaking rename of legacy V0.5 budget field. Pre-v1.0 allowed, but couples with audit work to avoid double-touch.

### Backlog (no version pin)

- **`workflow_templates` template-engine upgrade** (`templates/workflow_templates/mod.rs:19`) — only worth doing if presets ever gain conditionals; today `{{var}}` substitution suffices.
- **`preferences.toml` knob expansion** (`preferences.rs:8,43` + `commands.rs:4043` + `main.rs:627`) — additive, no current gap; expand on demand.
- **Test-fixture stub consolidation** — 10 `RecordingAdapter`-style impls in `crates/ccteam-imd/tests/*.rs` each redefine a near-identical fake adapter. A shared `tests/support/mock_adapter.rs` would deduplicate (each test crate compiles separately, so this needs `path = "..."` or a `test-util` feature). Not urgent.
