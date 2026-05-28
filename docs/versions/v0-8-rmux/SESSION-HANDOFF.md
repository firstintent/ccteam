# Session Handoff — rmux integration (v0-8-rmux-integration)

> Single current-state doc for migrating to a new session. **No-merge
> evaluation branch**: continuous development, no release, no PR.

## Current state (authoritative)

| 项 | 值 |
|---|---|
| 分支 | `v0-8-rmux-integration`(off origin/main `446e33a`),全部 pushed origin |
| HEAD | 见 `git log -1`(本 doc 写于 `562ea6b` 之后)|
| worktree | `/tmp/ccteam-rmux`(target 暖)|
| 默认 backend | **rmux**(库级单一 SoT — `from_env`/`default_backend`/`backend_kind_from_env`;仅显式 `CCTEAM_MUX_BACKEND=tmux` opt-out;unset/empty/typo → rmux)|
| 基线 | **1716 / 0**(`cargo test --workspace --locked --no-fail-fast --exclude ccteam-web`,clean run)· clippy 0 · fmt clean。本 WSL2 宿主在并发负载下偶现 1 flake = `daemon_wires_mock_channel_to_supervisor_inbox` / `daemon_dm_*`(inotify/timing env-race,`fs.inotify.max_user_instances=128` 易触顶,非回归,空载/CI 必过)|

## What's DONE in 0.8

- **W0–W7 rmux 集成**:MuxBackend trait + Tmux/Rmux/InProc 三 backend;单 binary daemon(`ccteam --__internal-daemon` re-exec + `RMUX_SDK_DAEMON_BINARY`);全 mode 走 trait;exit-empty=off + dead-handle reconnect;6 Codex wire 修复。
- **flip-default(库级)**:rmux 是 env-unset 默认,tmux 是 opt-out。21 个 tmux-fixture adapter 测试 + run_peek 单测 pin 到 tmux;positive rmux-adapter 覆盖(`claude_bg_rmux_adapter_test`)。**rmux 与 tmux 并存**:tmux backend + 全部 fixture 完整保留。
- **EnrichedEvent 类型化事件管线(Slice 1 + 2 + 3 + 4,flag-gated)** — register_pattern → PatternMatched → `TypedEventTap` → `EventMerger` → consumer(`typed_events.rs`)→ progress.jsonl:
  - **Slice 1**(`CCTEAM_TYPED_EVENTS=1`):no-enrichment kinds(rate_limit / context_overflow / idle / process_exited)→ `typed_event` 行。
  - **Slice 2**(再加 `CCTEAM_HOOK_VIA_DAEMON=1`):session→TapHandle registry(`Arc::ptr_eq`-guarded)把 W6 HookSink 的 Claude `Stop` hook 路由进对应 session 的 tap 作 `TurnDone` enrichment;`turn_done` pane 模式命中但 grace 窗口内无 hook → `BaseLossy` → `merger_lossy_partial` 行(可靠性兜底);`Paired` 抑制。
  - **Slice 3**(同 Slice 2 flag):`SeqState` 升级为 **time-windowed FIFO**(`PendingSlot { seq, arrived_at: tokio::time::Instant }` + `drop_stale` on opposite queue),消除 multi-in-flight cascade mis-pair;`enrich_kind_for_chat_action` 加 `tool-use` / `user-prompt` 映射。
  - **Slice 4**(同 Slice 2 flag):**identity-based cohort pairing** —`SeqState` 的 pending FIFO 由 `EventKind` 升级为 `(EventKind, Option<String>)` 复合 key,`BaseEvent` / `EnrichmentEvent` / `RawEnrichment` 加 `identity: Option<String>` 字段;`identity_for(kind, payload_json)` 从 hook payload 抽 `tool_name`。两个并发不同工具(`Edit` + `Read`)永不 cross-pair。同时 ship `pre-tool-use` 接线(`ToolCallStarted` mapping + `chat_tool_call_started` row)+ **Codex vendor parity**(新模块 `codex_typed_events.rs`:订阅 `CodexJsonRpcClient::subscribe()` 直接写 `progress.jsonl`,**绕过 merger** 以免无 base 永挂 `pending_enrichment` 累积;mode-3 only,mode-2 codex-exec 留到后续 slice)。另删除未使用的 `EventKind::TurnStarted`(与 `UserPromptSubmitted` 同源)。详 `w-slice-4-identity-and-codex.md`。
  - 默认路径(两 flag 关)完全不变 → baseline 不受影响。CI:`rmux-smoke.sh` 跑全部 `#[ignore]` 端到端测试(roundtrip + adapter + typed-event pipeline + Codex producer 模拟)。

## Remaining

- **macOS / Windows CI 绿 → 归 0.8.1**(硬件外因)。CI matrix 已接线(`rmux-smoke` on `[ubuntu-latest, macos-latest]`),push 即跑;本 Linux sandbox 无 Darwin/Windows runner。这是 merge-to-main 门槛(+ real-claude burn-in),非本环境任务。
- **within-grace identity mis-pair(同工具并发)**:Slice 4 消除了 cross-tool mis-pair;**same-tool** 并发(两个并行 Edit)在 grace 窗口内仍退化为 FIFO,可能 mis-pair。需要 `tool_use_id`-based pairing(等 Claude hook stdin schema 实地探测确认 `tool_use_id` 存在;repo 内无 first-hand 证据)。延 V0.8.1 探测后。
- **Codex 模式 2(`codex exec --json`)接管线 + Codex 基线 pattern wired**:V0.8 Slice 4 只接 mode-3 app-server enrichment 路径;mode-2 JSONL stdout + 基线 pane patterns 接 tap 是后续 slice。
- **`turn/plan/updated` → `PlanPending`** 语义在 Codex / Claude 不一致(Codex 是 `update_plan` todo-tool,Claude 是 plan-mode HITL);延后专项调和。

## 关键设计 / 红线

- rmux 是 crates.io `"0.3"` 依赖(可跟随升级,**非** path/git/vendor)。
- 红线:业务代码零 grep pane bytes(typed-event 走 daemon-side `subscribe` + vetted regex,不 scrape);`progress.jsonl` 是业务 event SoT;mode-3 对话原文走 `<project>/.ccteam/chat/<bot>/turns.jsonl`。
- 文档全在 `docs/versions/v0-8-rmux/` —— `as-built-architecture.md`(§5 typed-event 管线 + §8 flip-default gate)+ `w-flip-default-migration-plan.md` + `w-production-readiness.md`。

## 纪律(任何后续开发)

- 共享 worktree:subagent 只改自己 explicit-path 文件、**绝不** `git stash/checkout/add -A`;主代理做所有 commit。
- 每批后 `cargo clippy --workspace --all-targets --locked -- -D warnings`(0)+ `cargo fmt --all -- --check`(clean)+ push `git@github.com:firstintent/ccteam.git v0-8-rmux-integration:v0-8-rmux-integration`。
- 跑 `#[ignore]` rmux 测试会留 idle 守护进程(exit-empty off by design,unique socket,无害);大批后按 PID kill `/tmp/ccteam-rmux/target/debug/ccteam --__internal-daemon`(用 PID,别用 `pkill -f <pattern>` —— pattern 会 self-match 你的 shell 命令行),避免 inotify 触顶让 `daemon_dm_*` flake。

## 历史 GOAL prompt(原文,供参考)

```
新开一个rmux分支，分支不用发版，不用pr，在分支上连续开发即可，最终目标是整个rmux集成完成到100%，使用opus subagent。最大subagent不超过3个。直到所有功能完全就绪。达到用户生产级别。
```
后续补充:`要在0.8版本完全跑通rmux，并且rmux和tmux并存。不要放到0.9，本机无法实现的放到0.8.1中搞定`。

⚠️ 字面「100%」在 Linux sandbox 不可完全满足(macOS/Windows 生产验证需硬件 → 归 0.8.1)。**所有可在本环境验证的项已完成**:flip-default + rmux/tmux 并存 + EnrichedEvent 管线(Slice 1+2+3+4 含 identity pairing + Codex vendor parity)。

## 注意:`/goal` 是 session-scoped

迁到新 session **不会自动带 hook / goal**。要继续 goal-driven 开发,在新 session 重发 `/goal` 并贴本 doc;不重发 = 普通会话可正常收口。
