# V0.6.0 Wave 3 — Handoff

> **Status**: Shipping(integration branch `wave3-integration`,3-way merge done,baseline + clippy + 16 红线 全绿)。
> **Baseline**: 1282 / 1 — Wave 2 baseline 1201/1 + 81 net new tests(V0.5.1 942/1 累计 +340 net)。1 fail = pre-existing SSE flake。
> **Clippy**: 0 errors,20 warnings(+1 vs Wave 2 19,doc-list drift;非新逻辑)。
> **Wall time**: 3 teammate 并行 worktree ~1h(含 1 teammate follow-up commit);主 session 整合 + merge ~10 min(0 冲突 — 3 个 teammate 改 orchestrator.rs 的不同函数)。

## Decided

### F112 Codex Option B 完整

- **`CodexExecAdapter::submit_turn` + `resume_thread` 真 impl**(`codex exec --json -` stdin pipe + `codex resume <UUID> --json -` for resume;stdout JSONL → per-thread broadcast `ThreadEvent` stream)
- **`CodexAppServerAdapter`**(new `crates/ccteam-core/src/execution/codex_app_server.rs`)— 模式 3 codex via UDS JSON-RPC v2;dials `$CODEX_HOME/app-server-control/app-server-control.sock`(`CCTEAM_CODEX_APP_SERVER_SOCKET` env override);`thread/start` + `thread/resume` + `turn/start` + `thread/archive` 方法;`item/*` + `turn/*` notifications → `ThreadEvent` 翻译
- **`crates/ccteam-core/src/execution/codex_jsonrpc.rs`**(thin client)— line-delimited JSON-RPC-lite over UnixStream;channel-based writer task + reader-task demux(`oneshot` per request id,`broadcast` for notifications);`CodexJsonRpcClient::{connect_uds, spawn, call, subscribe, notify}` + `Notification` + `JsonRpcError`

### 4 用户可见场景(F112 §A-§D)

- **A. `/ccteam-advise`**(`skills/ccteam-advise/SKILL.md` ~190 行)— 并行调 Claude `Task` || Codex `Bash codex exec --json` → 合成 verdict(Claude/Codex/合成/分歧度 0-5)。codex 不可用时 Claude-only fallback。
- **B. Auto-critic in `ccteam-creator`** — Phase 3.5 detection。persona = critic / reviewer / architect 时,probe `codex --version && codex login status`,成功 → 写 `executor: codex` 到 rendered workflow.yaml(用户不见 yaml)。
- **C. `/ccteam-team` Codex critic teammate** — `/ccteam-team N "<task>"` N≥3 + codex 可用 → 自动 reserve 1 slot 给 `codex-critic`,Bash `codex exec --json &` 并行 spawn(Claude Code Task 不支持 target codex)。
- **D. opt-in fallback prefs** — `~/.ccteam/preferences.toml::fallback.on_claude_quota = "codex|off"`(default off);orchestrator `try_spawn_with_prompt` budget guard:Claude executor + Claude quota hit + prefs on + role eligible + Codex adapter registered → swap `effective_executor = codex` + emit `budget_exceeded { vendor: claude, vendor_fallback_to: codex }`;workflow 不停。
- **`ccteam prefs show|get|set <key> <value>` CLI** — admin CLI(`fallback.on_claude_quota` + `fallback.codex.enabled_for_roles` 逗号列表)。

### `ccteam doctor` codex 检查

- **`--check-codex-version`** — parse `codex --version`;≥0.131 ✓ / <0.131 ⚠ / missing ✗
- **`--check-codex-auth`** — parse `codex login status`;LoggedIn(ChatGPT|API key) ✓ / LoggedOut ⚠ / Unknown ⚠
- `parse_codex_semver` + `classify_codex_auth` 公开,供 skill 复用

### per-vendor budget caps

- `queries::CostSummary` 加 `cost_24h_by_vendor: BTreeMap<String, f64>` + `cost_total_by_vendor`(`#[serde(default)]` legacy 兼容)
- `agent_done` event 含 `vendor` 字段(从 `SessionHandle.harness` 推 via `vendor_from_harness` helper)
- `orchestrator::enforce_budget` 检查 `budgets_v060.{claude,codex}.max_cost_usd_per_24h` **before** legacy flat `spec.budget`;emit `budget_exceeded { kind: "cost_24h_per_vendor", vendor }`
- `ccteam-web::ProjectSummary` JSON 加 `cost_24h_by_vendor` 字段
- `translate_thread_event(usage, vendor)` 调 `ccteam_cost::estimate_cost(usage, vendor, "")`(model id 留 Wave 4 plumb through SpawnCtx)

### imd↔tui e2e wiring(Wave 2 推迟工作)

- **orchestrator mode:chat dispatch** — `pick_adapter(Executor, WorkflowMode)` 函数:claude+chat → `ClaudeTuiAdapter`,claude+bg → `ClaudeBgAdapter`,codex+chat → `CodexAppServerAdapter`(Wave 3 ship),codex+bg → `CodexExecAdapter`。`run_project_with_cancel` mode:chat 时早 return(ccteam-imd owns chat lifecycle,orchestrator skip event_loop)。
- **`ccteam-imd::supervisor` 真 HarnessAdapter 调用** — `BotSupervisor` 包 `Arc<dyn HarnessAdapter>` + `ThreadHandle`,methods:`ensure_started` / `handle_inbound` / `shutdown` / `restart` / `apply_action`。pure `decide()` 决定 SupervisorAction,`apply_action()` 桥接到 adapter trait 调用。**仅用 trait,无 `ccteam_core::execution::*` import**(R1 守)。
- **`SupervisorRegistry` + `register_supervisors_at_boot`**(e2e-wiring follow-up commit 35a3ec9)— `AdapterFactory` type + `default_adapter_factory()` 生产线;daemon::tick_supervisors 每 tick 一个 `BotSupervisor` per registered bot + apply `decide(state_snapshot)` action。
- **e2e mock TG test**(`crates/ccteam-imd/tests/e2e_mock_test.rs` ~500 行,5 测试)— stub `HarnessAdapter` 镜像 `ClaudeTuiAdapter` 语义(submit → 写 turns.jsonl);happy path + restart recovery + close cleanup + multi-bot parallel + channel listen 端到端。**真 tmux + 真 claude 不需要 — Wave 4 host probe 才跑**。

## Rejected

- ~~`unreachable!()` for codex+chat case~~ — e2e-wiring 选 fallback 到 `CodexExecAdapter` + TODO comment;codex-exec-impl ship CodexAppServerAdapter 后 swap arm(已在本 wave 同 PR 落)
- ~~ccteam-imd 调 ccteam-core API 通过 IPC 中转~~ — 直接走 `HarnessAdapter` trait + in-process AdapterFactory pattern,daemon 自己 own ThreadHandle per bot,**没 orchestrator ↔ daemon IPC** 复杂度
- ~~`codex exec` 一 thread 多 turn~~ — `codex exec --json` 是 one-shot;`submit_turn` 实施 = spawn 新子进程 per turn;`events()` 通过 per-thread broadcast 收当 turn 的 stdout JSONL stream;Wave 3 D2(`wave-3-decisions.md`)
- ~~codex-exec-impl 直接 commit 到 main~~ — `isolation: worktree` co-located with main worktree path,team-lead 救援:`git branch wave-3-e2e-wiring HEAD; git reset --hard origin/main` 把 commit 搬到 branch,teammate 在专属 branch 继续 follow-up

## Risks

- **真 codex resume e2e** 未跑(只 mocked via fake codex bash script `$CCTEAM_CODEX_BIN` env override)— Wave 4 host probe 跑真 codex `resume <UUID>` + verify cost tracking 准
- **model-id 不在 cost 估算流** — `translate_thread_event` 调 `estimate_cost(usage, vendor, "")` 用 fallback_model(Wave 3 D14)。Wave 4 plumb model id through `SpawnCtx` 让 cost ±5% 准
- **mode 3 codex bot 未启** — Wave 1 决策 mode 3 V0.6 仅 Claude bot;`CodexAppServerAdapter` 已 ship 让 trait stack 统一,V0.7 启 codex bot 时零代码改动 daemon-side
- **CodexAppServerAdapter notifications → progress.jsonl bridge 未通**(Wave 3 D9)— events() stream 当前只 SSE 端消费;Wave 4 加 daemon-side bridge
- **clippy 20 warnings**(+1 vs Wave 2 19)— Wave 3 加了 doc-overindented-list 等 1 个新 warning(同 doc-list drift category)。Wave 4 doc-sweep chore 一次性清

## Files

新文件(20+):
- `crates/ccteam-core/src/{auto_critic, preferences}.rs`
- `crates/ccteam-core/src/execution/{codex_app_server, codex_jsonrpc}.rs`
- `crates/ccteam-core/tests/{auto_critic, preferences, fallback_quota, codex_exec_wave3, codex_app_server, codex_jsonrpc, per_vendor_budget}_test.rs`
- `crates/ccteam-cli/tests/doctor_codex_test.rs`
- `crates/ccteam-imd/tests/e2e_mock_test.rs`
- `skills/ccteam-advise/SKILL.md`
- `docs/versions/v0-6-0/{wave-3-decisions, wave-3-handoff}.md`

修改:
- `crates/ccteam-core/src/{orchestrator, queries, lib}.rs`(per-vendor budget + mode:chat dispatch + re-exports)
- `crates/ccteam-core/src/execution/codex_exec.rs`(Wave 1 stub → 真 impl)
- `crates/ccteam-cli/src/{commands, main}.rs`(prefs CLI + doctor flags)
- `crates/ccteam-imd/src/{daemon, supervisor, outbound, main}.rs`(SupervisorRegistry + AdapterFactory + real HarnessAdapter calls)
- `crates/ccteam-web/src/routes/api_v1.rs`(cost_24h_by_vendor field)
- `skills/ccteam-creator/SKILL.md` + `skills/ccteam-team/SKILL.md`(auto-critic + codex critic teammate)

## Remaining(Wave 4)

- **5 preset full host probe**(Solo Sidekick / Team Sprint / Overnight Builder / Pocket Assistant / IM Squad)+ 录 5 demo GIF 放 `docs/versions/v0-6-0/demos/`
- **真 TG e2e probe**:user 已 paste token + `/start`'d bot;Wave 4 跑完整 mode 3 hello world
- **真 Codex host probe**:`/ccteam-advise` + auto-critic + Codex critic teammate + opt-in fallback 各 1 个 host run
- **`docs/tech-design.md` + `interfaces.md` + `CLAUDE.md` 同步**(F106 集成部分 + workspace version 0.5.1 → 0.6.0 + baseline 数字回填)
- **`docs/dev-coupling-audit.md`** F106-F118 各 1 finding
- **Tier-1 docs MCP tool name 子前缀 sweep**(Wave 1 skill-mcp 标的 finding)— `docs/interfaces.md` + `docs/claude-code-tool-surface.md` + `docs/versions/v0-1/user-quickstart.md` + `skills/ccteam-control/SKILL.md`
- **clippy doc-list drift sweep** — 20 warnings → 0(独立 chore PR scope,Wave 4 顺便)
- **model-id plumb through SpawnCtx**(Wave 3 D14)— 让 cost 估算 ±5% 准而非 fallback_model
- **git tag v0.6.0**(user 决策:仅 tag,no GitHub release page)
