# W3b — Codex app-server Event Catalog

> Status: research artifact for V0.8 rmux integration. **Read-only catalog**, no code change recommendation goes in this file beyond §4 / §7 / §8.
>
> Purpose: be the canonical reference for every event/notification/server-request the Codex `app-server` emits over its JSON-RPC UDS, so W3b (Codex EnrichedEvent merger) and W4 (Codex mode 3b in rmux PTY) can choose which surfaces to consume without re-reading 23 kLOC of upstream protocol code.
>
> All file paths are absolute and rooted at `/tmp/ccteam-rmux/` (worktree of branch `v0-8-rmux-integration`).

---

## §1 Codex app-server protocol overview

### Source pin

| Item | Value |
|---|---|
| Upstream tree | `/tmp/ccteam-rmux/references/codex/codex-rs/` (symlinked) |
| Pinned commit | `76845d716b "Deduplicate issue digest interactions by user (#22039)"` (2026-05-10) |
| Crate version | `codex-app-server-protocol` uses `version.workspace = true`; workspace `Cargo.toml` reports `0.0.0` for the v8-poc fork tree. The OSS-published `codex` CLI ccteam dials at runtime is whatever the user has on `$PATH` (`codex --version`). |
| Protocol crate | `references/codex/codex-rs/app-server-protocol/` |
| Server crate | `references/codex/codex-rs/app-server/` |
| Method-registry source of truth | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:434-1033` (`client_request_definitions!`), `:1277-1341` (`server_request_definitions!`), `:1425-1517` (`server_notification_definitions!`) |

### Transport + framing

| Aspect | Detail | Reference |
|---|---|---|
| Wire transport | Unix Domain Socket (Codex also supports `stdio-to-uds` bridging); ccteam dials UDS only | `references/codex/codex-rs/app-server/src/transport.rs`, `references/codex/codex-rs/stdio-to-uds/` |
| Default UDS path | `$CODEX_HOME/app-server-control/app-server-control.sock` (`CODEX_HOME` falls back to `~/.codex`) | `crates/ccteam-core/src/execution/codex_app_server.rs:118-129` |
| ccteam env override | `CCTEAM_CODEX_APP_SERVER_SOCKET` | `crates/ccteam-core/src/execution/codex_app_server.rs:60` |
| Framing | **Line-delimited JSON, "JSON-RPC 2.0 lite"** — no `jsonrpc: "2.0"` discriminator; `\n`-terminated frames | `crates/ccteam-core/src/execution/codex_jsonrpc.rs:1-25`, `references/codex/codex-rs/app-server-protocol/src/jsonrpc_lite.rs` |
| Request frame | `{ "id": <i64>, "method": "<m>", "params": <obj?> }` | same |
| Response frame | `{ "id": <i64>, "result": <obj> }` OR `{ "id": <i64>, "error": { "code": <i32>, "message": <str>, "data": <any?> } }` | same |
| Notification frame | `{ "method": "<m>", "params": <obj> }` (no `id`) | same |
| Server→client request | Same as client→server request shape; client must reply with `{"id":<same>,"result":<obj>}` | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1053-1088` |
| Concurrent in-flight | id-keyed; ccteam's `CodexJsonRpcClient` uses oneshot map keyed by `i64` id | `crates/ccteam-core/src/execution/codex_jsonrpc.rs:47, 100-127` |

### Client connection lifecycle (Codex-prescribed)

1. **Connect** UDS.
2. **`initialize`** request with `InitializeParams { client_info, capabilities }`. The response carries `user_agent`, `codex_home`, `platform_family`, `platform_os`.
   - Source: `references/codex/codex-rs/app-server-protocol/src/protocol/v1.rs:28-70`
   - **`InitializeCapabilities` is load-bearing** (`v1.rs:44-56`):
     - `experimental_api: bool` — opt into experimental methods + experimental notification variants. Default `false`.
     - `request_attestation: bool` — opt into `attestation/generate` server-initiated requests.
     - `opt_out_notification_methods: Option<Vec<String>>` — wire method names this connection wants suppressed (e.g. `"thread/started"`). Daemon-side filter is **cheaper** than ccteam-side filter.
3. **`Initialized`** client notification (one-way) signals readiness to receive server-initiated requests and notifications. (`client_notification_definitions!{ Initialized }` at `common.rs:1519-1521`.)
4. **Per-thread lifecycle**: `thread/start` → many `turn/start` + their notifications → `thread/unsubscribe`/`thread/archive` on close.

**Important ccteam gap (see §7 + §8)**: `crates/ccteam-core/src/execution/codex_app_server.rs::start_thread()` calls `thread/start` directly without an `initialize` handshake. The doc-comment at line 10 says `"initialize (if needed) → thread/start"` but the code path **never sends `initialize`** and never sends the `Initialized` notification either. Consequence: ccteam runs with `experimental_api = false` by default → the experimental notifications listed in §2 (notably `turn/plan/updated`, `item/plan/delta`, `thread/goal/*`, `process/*`, `thread/realtime/*`) are **never delivered**. W3b must add this handshake before relying on those events.

---

## §2 Complete event / notification / request inventory

### §2.1 Server → Client notifications (no response expected)

Authoritative source: `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1425-1517` (`server_notification_definitions!` macro block). Payload structs live in `protocol/v2/<area>.rs`.

Layer column maps to research doc §14.3 (`docs/research/embedded-mux-unified-architecture.md`):
- L1 = process-level signal
- L2 = surface pattern (rate-limit, idle)
- L3 = conversation content (assistant text)
- L4 = semantic event (tool call, plan tree, decisions)
- L0 = control-plane (connection-level metadata; not in research §14 taxonomy but called out below)

| Wire method | Payload struct (file) | Key params | Layer | ccteam mode-3 consumed? | progress.jsonl mapping |
|---|---|---|---|---|---|
| `thread/started` | `ThreadStartedNotification` (v2/thread.rs:1122) | `thread: Thread` (carries thread_id, cwd, model, ...) | L0 | **Yes** (codex_app_server.rs:477) | `agent_spawn` (orchestrator-emitted on thread start; bridge does not write this from notification) |
| `thread/status/changed` | `ThreadStatusChangedNotification` (v2/thread.rs:1129) | `thread_id`, `status: ThreadStatus` (NotLoaded/Idle/SystemError/Active{active_flags:[WaitingOnApproval,WaitingOnUserInput]}) | L4 | **No** — dead-end at `translate_notification` "other" arm | — (orchestrator polls thread state separately; HITL state is currently inferred elsewhere) |
| `thread/archived` | `ThreadArchivedNotification` (v2/thread.rs:1137) | `thread_id` | L0 | No | — |
| `thread/unarchived` | `ThreadUnarchivedNotification` (v2/thread.rs:1144) | `thread_id` | L0 | No | — |
| `thread/closed` | `ThreadClosedNotification` (v2/thread.rs:1151) | `thread_id` | L0 | No | — |
| `thread/name/updated` | `ThreadNameUpdatedNotification` (v2/thread.rs:1157) | `thread_id`, `thread_name?` | L0 | No | — |
| `thread/goal/updated` *(experimental)* | `ThreadGoalUpdatedNotification` (v2/thread.rs:1167) | `thread_id`, `turn_id?`, `goal: ThreadGoal {objective, status, token_budget, tokens_used, time_used_seconds, ...}` | L4 | No (gated by `experimental_api`) | — |
| `thread/goal/cleared` *(experimental)* | `ThreadGoalClearedNotification` (v2/thread.rs:1176) | `thread_id` | L4 | No | — |
| `thread/tokenUsage/updated` | `ThreadTokenUsageUpdatedNotification` (v2/thread.rs:1063) | `thread_id`, `turn_id`, `token_usage: ThreadTokenUsage { total: TokenUsageBreakdown, last: TokenUsageBreakdown, model_context_window?}` | L4 | No | — (cost rollup currently lands at `turn/completed` only; this gives mid-turn deltas) |
| `thread/compacted` *(deprecated; superseded by `ContextCompaction` item)* | `ContextCompactedNotification` (v2/thread.rs:1184) | `thread_id`, `turn_id` | L4 | No | — |
| `skills/changed` | `SkillsChangedNotification` (v2/plugin.rs:776, empty struct) | — | L0 | No | — |
| `turn/started` | `TurnStartedNotification` (v2/turn.rs:312) | `thread_id`, `turn: Turn` | L4 | **Yes** (codex_app_server.rs:485) | — (translated to `ThreadEvent::TurnStarted` only; not mirrored to progress.jsonl) |
| `turn/completed` | `TurnCompletedNotification` (v2/turn.rs:329) | `thread_id`, `turn: Turn` (carries usage breakdown) | L4 | **Yes** (codex_app_server.rs:494) | `agent_done` with `status: "completed"`, `vendor: "codex"`, `cost_usd` (codex_app_server.rs:582-601) |
| `turn/diff/updated` | `TurnDiffUpdatedNotification` (v2/turn.rs:339) | `thread_id`, `turn_id`, `diff: String` (aggregated unified diff for the whole turn) | L4 | No | — |
| `turn/plan/updated` | `TurnPlanUpdatedNotification` (v2/turn.rs:348) | `thread_id`, `turn_id`, `explanation?`, `plan: Vec<TurnPlanStep{step, status: Pending\|InProgress\|Completed}>` | L4 | No | — (Claude analog is `plan_pending` event; see §3) |
| `hook/started` | `HookStartedNotification` (v2/hook.rs:141) | `thread_id`, `turn_id?`, `run: HookRunSummary` | L4 | No | — |
| `hook/completed` | `HookCompletedNotification` (v2/hook.rs:150) | `thread_id`, `turn_id?`, `run: HookRunSummary` | L4 | No | — |
| `item/started` | `ItemStartedNotification` (v2/item.rs:1059) | `item: ThreadItem`, `thread_id`, `turn_id`, `started_at_ms: i64` | L4 | **Yes** (codex_app_server.rs:522) | — (translated to `ThreadEvent::ItemStarted`; not mirrored) |
| `item/completed` | `ItemCompletedNotification` (v2/item.rs:1133) | `item: ThreadItem`, `thread_id`, `turn_id`, `completed_at_ms: i64` | L4 | **Yes** (codex_app_server.rs:528) | — (translated to `ThreadEvent::ItemCompleted`; not mirrored) |
| `item/agentMessage/delta` | `AgentMessageDeltaNotification` (v2/item.rs:1155) | `thread_id`, `turn_id`, `item_id`, `delta: String` | L3 | **Yes** (codex_app_server.rs:531) | — |
| `item/plan/delta` *(experimental)* | `PlanDeltaNotification` (v2/item.rs:1167) | `thread_id`, `turn_id`, `item_id`, `delta: String` | L4 | No (experimental) | — |
| `item/reasoning/summaryTextDelta` | `ReasoningSummaryTextDeltaNotification` (v2/item.rs:1177) | `thread_id`, `turn_id`, `item_id`, `delta`, `summary_index: i64` | L3 | No | — |
| `item/reasoning/summaryPartAdded` | `ReasoningSummaryPartAddedNotification` (v2/item.rs:1189) | `thread_id`, `turn_id`, `item_id`, `summary_index` | L3 | No | — |
| `item/reasoning/textDelta` | `ReasoningTextDeltaNotification` (v2/item.rs:1200) | `thread_id`, `turn_id`, `item_id`, `delta`, `content_index: i64` | L3 | No | — |
| `item/commandExecution/outputDelta` | `CommandExecutionOutputDeltaNotification` (v2/item.rs:1224) | `thread_id`, `turn_id`, `item_id`, `delta: String` | L4 | No | — |
| `item/commandExecution/terminalInteraction` | `TerminalInteractionNotification` (v2/item.rs:1212) | `thread_id`, `turn_id`, `item_id`, `process_id`, `stdin: String` | L4 | No | — |
| `item/fileChange/outputDelta` *(deprecated)* | `FileChangeOutputDeltaNotification` (v2/item.rs:1236) | `thread_id`, `turn_id`, `item_id`, `delta` | L4 | No | — (server "no longer emits" per v2/item.rs:1232) |
| `item/fileChange/patchUpdated` | `FileChangePatchUpdatedNotification` (v2/item.rs:1246) | `thread_id`, `turn_id`, `item_id`, `changes: Vec<FileUpdateChange>` | L4 | No | — |
| `item/mcpToolCall/progress` | `McpToolCallProgressNotification` (v2/mcp.rs:202) | `thread_id`, `turn_id`, `item_id`, `message: String` | L4 | No | — |
| `item/autoApprovalReview/started` *(unstable)* | `ItemGuardianApprovalReviewStartedNotification` (v2/item.rs:1073) | `thread_id`, `turn_id`, `started_at_ms`, `review_id`, `target_item_id?`, `review: GuardianApprovalReview { status, risk_level?, user_authorization?, rationale? }`, `action: GuardianApprovalReviewAction` | L4 | No | — |
| `item/autoApprovalReview/completed` *(unstable)* | `ItemGuardianApprovalReviewCompletedNotification` (v2/item.rs:1102) | as above + `completed_at_ms`, `decision_source: AutoReviewDecisionSource` | L4 | No | — |
| `rawResponseItem/completed` *(internal/Codex-Cloud only)* | `RawResponseItemCompletedNotification` (v2/item.rs:1145) | `thread_id`, `turn_id`, `item: ResponseItem` | L4 | No | — (only when `experimental_raw_events: true` set on `thread/start`) |
| `command/exec/outputDelta` | `CommandExecOutputDeltaNotification` (v2/command_exec.rs:203) | `process_id`, `stream: Stdout\|Stderr`, `delta_base64`, `cap_reached: bool` | L1 | No | — (only for standalone `command/exec` requests, not turn execs) |
| `process/outputDelta` *(experimental)* | `ProcessOutputDeltaNotification` (v2/process.rs:165) | `process_handle`, `stream`, `delta_base64`, `cap_reached` | L1 | No | — |
| `process/exited` *(experimental)* | `ProcessExitedNotification` (v2/process.rs:181) | `process_handle`, `exit_code: i32`, `stdout`, `stdout_cap_reached`, `stderr`, `stderr_cap_reached` | L1 | No | — |
| `mcpServer/oauthLogin/completed` | `McpServerOauthLoginCompletedNotification` (v2/mcp.rs:212) | `name`, `success`, `error?` | L0 | No | — |
| `mcpServer/startupStatus/updated` | `McpServerStatusUpdatedNotification` (v2/mcp.rs:233) | `name`, `status: Starting\|Ready\|Failed\|Cancelled`, `error?` | L0 | No | — |
| `account/updated` | `AccountUpdatedNotification` (v2/account.rs:242) | `auth_mode?`, `plan_type?` | L0 | No | — |
| `account/login/completed` | `AccountLoginCompletedNotification` (v2/account.rs:377) | `login_id?`, `success`, `error?` | L0 | No | — |
| `account/rateLimits/updated` | `AccountRateLimitsUpdatedNotification` (v2/account.rs:250) | `rate_limits: RateLimitSnapshot` | L2 | No | — (this is Codex's native rate-limit visibility; replaces TUI scrape) |
| `app/list/updated` *(experimental)* | `AppListUpdatedNotification` (v2/apps.rs:144) | `data: Vec<AppInfo>` | L0 | No | — |
| `remoteControl/status/changed` | `RemoteControlStatusChangedNotification` (v2/remote_control.rs:10) | `status: Disabled\|Connecting\|Connected\|Errored`, `installation_id`, `environment_id?` | L0 | No | — |
| `externalAgentConfig/import/completed` | `ExternalAgentConfigImportCompletedNotification` (v2/config.rs:634, empty struct) | — | L0 | No | — |
| `fs/changed` | `FsChangedNotification` (v2/fs.rs:199) | `watch_id`, `changed_paths: Vec<AbsolutePathBuf>` | L1 | No | — (only for active `fs/watch` subscriptions) |
| `model/rerouted` | `ModelReroutedNotification` (v2/model.rs:136) | `thread_id`, `turn_id`, `from_model`, `to_model`, `reason: ModelRerouteReason` | L4 | No | — |
| `model/verification` | `ModelVerificationNotification` (v2/model.rs:147) | `thread_id`, `turn_id`, `verifications: Vec<ModelVerification>` | L4 | No | — |
| `warning` | `WarningNotification` (v2/notification.rs:21) | `thread_id?`, `message` | L2 | No | — |
| `guardianWarning` | `GuardianWarningNotification` (v2/notification.rs:31) | `thread_id`, `message` | L2 | No | — |
| `error` | `ErrorNotification` (v2/notification.rs:41) | `error: TurnError`, `will_retry: bool`, `thread_id`, `turn_id` | L4 | **Partial** — only when method-name is `turn/failed` (ccteam never reads `"error"` notification); this is a different code path from the `"error"` event-type in mode-2 exec | — |
| `deprecationNotice` | `DeprecationNoticeNotification` (v2/notification.rs:11) | `summary`, `details?` | L0 | No | — |
| `configWarning` | `ConfigWarningNotification` (v2/config.rs:695) | `summary`, `details?`, `path?`, `range?` | L0 | No | — |
| `serverRequest/resolved` | `ServerRequestResolvedNotification` (v2/notification.rs:53) | `thread_id`, `request_id: RequestId` | L0 | No | — (signals that a previously-issued server-initiated request has been finalised by some peer) |
| `fuzzyFileSearch/sessionUpdated` | `FuzzyFileSearchSessionUpdatedNotification` (common.rs:1409) | `session_id`, `query`, `files: Vec<FuzzyFileSearchResult>` | L0 | No | — |
| `fuzzyFileSearch/sessionCompleted` | `FuzzyFileSearchSessionCompletedNotification` (common.rs:1421) | `session_id` | L0 | No | — |
| `windows/worldWritableWarning` | `WindowsWorldWritableWarningNotification` (v2/windows_sandbox.rs:10) | `sample_paths: Vec<String>`, `extra_count: usize`, `failed_scan: bool` | L0 | No | — |
| `windowsSandbox/setupCompleted` | `WindowsSandboxSetupCompletedNotification` (v2/windows_sandbox.rs:59) | (struct fields not relevant for unix host) | L0 | No | — |
| `thread/realtime/started` *(experimental)* | `ThreadRealtimeStartedNotification` (v2/realtime.rs:168) | thread + realtime session id | L4 | No | — (voice / realtime API; not in scope for ccteam) |
| `thread/realtime/itemAdded` *(experimental)* | `ThreadRealtimeItemAddedNotification` (v2/realtime.rs:178) | | L4 | No | — |
| `thread/realtime/transcript/delta` *(experimental)* | `ThreadRealtimeTranscriptDeltaNotification` (v2/realtime.rs:188) | | L3 | No | — |
| `thread/realtime/transcript/done` *(experimental)* | `ThreadRealtimeTranscriptDoneNotification` (v2/realtime.rs:200) | | L3 | No | — |
| `thread/realtime/outputAudio/delta` *(experimental)* | `ThreadRealtimeOutputAudioDeltaNotification` (v2/realtime.rs:211) | | L3 | No | — |
| `thread/realtime/sdp` *(experimental)* | `ThreadRealtimeSdpNotification` (v2/realtime.rs:220) | | L0 | No | — |
| `thread/realtime/error` *(experimental)* | `ThreadRealtimeErrorNotification` (v2/realtime.rs:229) | | L4 | No | — |
| `thread/realtime/closed` *(experimental)* | `ThreadRealtimeClosedNotification` (v2/realtime.rs:238) | | L4 | No | — |

**Bare counts**: 50 notification variants (1 deprecated, 8 realtime-only, 12 experimental-only, leaving ~29 stable non-realtime).

### §2.2 Server → Client requests (expect a response; HITL & elicitation surface)

Source: `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1277-1341` (`server_request_definitions!`). These are *not* notifications — the server waits for a `{"id":<same>,"result":<obj>}` reply and the turn blocks until it lands.

| Wire method | Params struct | Response struct | What it signals | ccteam consumes? |
|---|---|---|---|---|
| `item/commandExecution/requestApproval` | `CommandExecutionRequestApprovalParams` (v2/item.rs:1256) | `CommandExecutionRequestApprovalResponse` (decision enum: Accept/AcceptForSession/AcceptWithExecpolicyAmendment/Decline/Cancel/ApplyNetworkPolicyAmendment) | Codex wants user approval for a single command exec inside a `turn/start` turn | **No** — ccteam never replies, so any HITL-required Codex turn currently stalls until timeout (see §8) |
| `item/fileChange/requestApproval` | `FileChangeRequestApprovalParams` | `FileChangeRequestApprovalResponse` (decision: Accept/AcceptForSession/Decline/Cancel) | Codex wants approval for a proposed file edit (apply_patch surface in v2) | **No** |
| `item/tool/requestUserInput` *(experimental)* | `ToolRequestUserInputParams` | `ToolRequestUserInputResponse` | Tool needs free-form input from user | **No** |
| `mcpServer/elicitation/request` | `McpServerElicitationRequestParams` | `McpServerElicitationRequestResponse` (action: Accept/Decline/Cancel) | An MCP server (via Codex) is eliciting input | **No** |
| `item/permissions/requestApproval` | `PermissionsRequestApprovalParams` | `PermissionsRequestApprovalResponse` | Codex wants to broaden the sandbox/permission profile mid-turn | **No** |
| `item/tool/call` | `DynamicToolCallParams` | `DynamicToolCallResponse` | Codex is invoking a dynamic-tool the *client* declared in `thread/start.dynamicTools` | **No** (ccteam doesn't declare dynamic tools) |
| `account/chatgptAuthTokens/refresh` | `ChatgptAuthTokensRefreshParams` | `ChatgptAuthTokensRefreshResponse` | Server needs the client to refresh its ChatGPT OAuth tokens | **No** |
| `attestation/generate` | `AttestationGenerateParams` | `AttestationGenerateResponse` | Server wants a fresh upstream attestation (only when `request_attestation: true` in initialize) | **No** (capability disabled by default) |
| `applyPatchApproval` *(deprecated v1 path)* | `ApplyPatchApprovalParams` (v1.rs:129) | `ApplyPatchApprovalResponse` (decision: ReviewDecision) | Legacy approval for v1 SendUserTurn/SendUserMessage path | **No** |
| `execCommandApproval` *(deprecated v1 path)* | `ExecCommandApprovalParams` (v1.rs:150) | `ExecCommandApprovalResponse` | Legacy exec approval for v1 path | **No** |

### §2.3 Client → Server requests (for completeness)

The full request inventory has ~80 methods — full list at `common.rs:434-1033`. Stable bucket prefixes:
- `thread/{start, resume, fork, archive, unarchive, unsubscribe, list, loaded/list, read, rollback, name/set, metadata/update, inject_items, compact/start, shellCommand, approveGuardianDeniedAction, turns/list, turns/items/list}` — thread lifecycle
- `turn/{start, steer, interrupt}` — turn lifecycle
- `command/exec{, /write, /terminate, /resize}` — standalone sandboxed command exec
- `process/{spawn, writeStdin, kill, resizePty}` *(experimental)* — unsandboxed PTY-able process
- `fs/{readFile, writeFile, createDirectory, getMetadata, readDirectory, remove, copy, watch, unwatch}` — remote filesystem
- `config/{read, value/write, batchWrite, mcpServer/reload}` + `configRequirements/read` + `experimentalFeature/list,enablement/set`
- `mcpServer/{oauth/login, resource/read, tool/call}` + `mcpServerStatus/list`
- `account/{login/start, login/cancel, logout, read, rateLimits/read, sendAddCreditsNudgeEmail}` + `getAuthStatus` (deprecated v1)
- `plugin/{list, read, install, uninstall, share/{save,updateTargets,list,delete}, skill/read}` + `marketplace/{add,remove,upgrade}`
- `feedback/upload`
- `skills/{list, config/write}`, `hooks/list`, `app/list`, `model/list`, `modelProvider/capabilities/read`
- `review/start`, `collaborationMode/list`
- `environment/add` *(experimental)*, `memory/reset` *(experimental)*
- `windowsSandbox/{setupStart, readiness}`
- Realtime *(experimental)*: `thread/realtime/{start, stop, listVoices, appendAudio, appendText}`
- Fuzzy file search: `fuzzyFileSearch{, /sessionStart, /sessionUpdate, /sessionStop}`
- Mock for tests: `mock/experimentalMethod`
- Deprecated v1: `getConversationSummary`, `gitDiffToRemote`, `getAuthStatus`

ccteam mode-3 sends only `thread/start`, `thread/resume`, `thread/archive`, `thread/unsubscribe`, `turn/start` today (see §7 for the `initialize`/`Initialized` gap).

### §2.4 Client → Server notifications

Source: `common.rs:1519-1521` (`client_notification_definitions!`).
- `Initialized` — one-way readiness signal after `initialize` response. (No payload.)

Nothing else — the client cannot push notifications to the server today.

---

## §3 Codex-only events with no Claude analog

These are signals ccteam gets **for free** from Codex's typed JSON-RPC channel that have no clean equivalent on the Claude side (where ccteam currently uses Anthropic transcript jsonl + Claude Code hooks + rmux PatternMatched — none of which produce these structures).

| Codex signal | What it gives | Claude best-effort | Why Codex wins |
|---|---|---|---|
| `turn/plan/updated` (`Vec<TurnPlanStep{step,status: Pending/InProgress/Completed}>`) | Structured plan tree with per-step status | ccteam fakes this from Claude Code `pre_tool_use` hook (`TodoWrite` calls); status enum is approximate | Codex emits the canonical typed plan tree directly |
| `thread/tokenUsage/updated` (per-turn `total + last + context_window`) | Live mid-turn token accounting | Claude exposes usage only at `turn_done` via hook | Codex gives **mid-turn** deltas → budget tripwires can fire before a runaway turn completes |
| `turn/diff/updated` (`diff: String`, whole-turn aggregated unified diff) | One canonical diff covering every file the turn touched | ccteam would have to `git diff` from outside or aggregate per-file Claude tool results | Codex aggregates server-side |
| `model/rerouted` (`{from_model, to_model, reason}`) | When the server fails-over to a different model | No Claude equivalent — invisible in ccteam today | Pure free-find for Codex |
| `model/verification` | Mid-turn model verification events | No Claude equivalent | Pure free-find for Codex |
| `thread/status/changed` with `ThreadActiveFlag::{WaitingOnApproval, WaitingOnUserInput}` | Authoritative "this thread is stuck waiting on a human" state | ccteam infers Claude HITL from absent activity + plan-approval hook | Codex says it explicitly |
| `account/rateLimits/updated` | Codex's native rate-limit visibility | ccteam currently learns of Claude rate-limit by scraping the TUI error string (Layer-2 pattern) | Typed numeric data → cleaner budget cap logic |
| `item/autoApprovalReview/{started,completed}` (Guardian) | Server-side automatic policy review with risk_level + rationale | Claude has no such surface | Codex-only feature |
| `ThreadItem::CollabAgentToolCall` (inside `item/*`) | Codex's multi-agent collab tool calls with `sender_thread_id` + `receiver_thread_ids` + `agents_states` | Claude `Task()` subagent calls are visible in hooks but not as a typed cross-thread reference | Better for V0.7 sub-agent observability |
| `ThreadItem::Reasoning` + `item/reasoning/*Delta` (3 variants) | Distinguished reasoning vs. final-message channels with separate text + summary streams | Claude blurs these in transcript jsonl (just "thinking" text blocks) | Cleaner separation |
| `ThreadItem::ContextCompaction` | Typed marker that auto-compaction happened mid-thread | Claude `/compact` is detectable, but spontaneous Anthropic-side auto-compact isn't | — |
| `ThreadItem::EnteredReviewMode` / `ExitedReviewMode` | Server-tracked review-mode entry/exit | Claude has no review-mode primitive | — |
| Server-initiated `item/permissions/requestApproval` | Typed HITL ask for sandbox-broadening with the full proposed permission delta | Claude's plan_pending hook gives intent but not the precise permission delta | — |
| `hook/started` + `hook/completed` (Codex hooks) | Codex's own hook lifecycle (parallel to Claude Code hooks but server-side, typed) | n/a | Useful only when running Codex hooks; not relevant unless ccteam ships Codex hook templates |
| `fs/changed` (when `fs/watch` is active) | Server-side filesystem watch | Claude has no equivalent — rmux/inotify is the only path | Useful when Codex is running on a remote host (sandbox/cloud) where ccteam can't directly inotify |

**Take-away**: Codex's protocol is **strictly richer at Layer 3-4** than what ccteam can extract from Claude today. The structured plan tree, mid-turn token deltas, and aggregated turn diff are the three "crown jewels" that motivate W3b. Anything ccteam needs to know about a Codex thread can be answered by JSON-RPC events; nothing forces TUI scraping.

---

## §4 Canonical subset for W4 mode-3b consumption

> **W4 mounts Codex inside the rmux PTY (mode 3b).** The PTY emits the Codex TUI bytes (which we mostly ignore — see §6); the typed event stream is consumed via the daemon's `CodexUdsBridge` connecting to the same `app-server` UDS. This section recommends which notifications the bridge should *subscribe* to vs. *opt out of at handshake*.

### §4.1 Prereq: complete the `initialize` handshake

Before consuming experimental events, the W3b adapter MUST send:

```jsonc
// 1) initialize request
{ "id": 1, "method": "initialize", "params": {
    "clientInfo": { "name": "ccteam", "version": "<workspace>" },
    "capabilities": {
        "experimentalApi": true,                // unlocks turn/plan/updated, item/plan/delta, thread/goal/*, process/*, thread/realtime/* (gate at common.rs registration sites)
        "requestAttestation": false,            // we don't proxy attestation today
        "optOutNotificationMethods": [          // server-side filter is cheaper than ccteam-side
            "thread/realtime/started",
            "thread/realtime/itemAdded",
            "thread/realtime/transcript/delta",
            "thread/realtime/transcript/done",
            "thread/realtime/outputAudio/delta",
            "thread/realtime/sdp",
            "thread/realtime/error",
            "thread/realtime/closed",
            "windows/worldWritableWarning",
            "windowsSandbox/setupCompleted",
            "app/list/updated",
            "skills/changed",
            "fuzzyFileSearch/sessionUpdated",
            "fuzzyFileSearch/sessionCompleted",
            "remoteControl/status/changed"
        ]
    }
}}

// 2) Initialized notification
{ "method": "initialized" }
```

The capability check is gated at `protocol/common.rs:EXPERIMENTAL_CLIENT_METHODS` const (auto-generated by the `client_request_definitions!` macro). Without `experimentalApi: true` ccteam never sees `turn/plan/updated`, `item/plan/delta`, `thread/goal/*`, `process/*`, `command/exec.permissionProfile`, `thread/realtime/*`, `environment/add`, `memory/reset`, `mock/*`, or any `experimental_raw_events` notification — verified at `protocol/common.rs:434-1033` `#[experimental(...)]` annotations.

### §4.2 Business-critical (must consume)

| Wire method | Why | Drives |
|---|---|---|
| `thread/started` | Confirms thread_id binding | `agent_spawn` row (already done by orchestrator; bridge can stay no-op here) |
| `turn/started` | Marks turn boundary | Per-turn timeout watchdog reset (F195) |
| `turn/completed` | Carries `turn.usage` for cost rollup | `agent_done {status:"completed", vendor:"codex", cost_usd, usage}` (already done — codex_app_server.rs:582) |
| `turn/failed` *(emitted via `error` notification — see §8 note)* | Failure surface | `agent_done {status:"errored"}` (already done) |
| `turn/plan/updated` | Structured plan tree | `plan_pending` (V0.6.1 F98) parity with Claude |
| `thread/tokenUsage/updated` | Mid-turn token deltas | Budget tripwire pre-turn-end (currently fire only at turn boundary) |
| `thread/status/changed` | `ThreadActiveFlag::{WaitingOnApproval, WaitingOnUserInput}` | HITL detection without scraping; F124 `mode: human-approval` adapter wiring |
| `account/rateLimits/updated` | Typed rate-limit instead of TUI scrape | Budget-cap escalation (F84 auto-disable) |
| `error` | Includes `will_retry: bool` so we can distinguish transient retries from terminal failures | Avoid prematurely escalating retryable errors |
| `item/started` + `item/completed` | Tool-call / file-change / web-search / reasoning surface | `tool_added` (F128); item-level summaries for chat UI |
| `item/agentMessage/delta` | Streaming assistant text | Live token streaming to IM/web for codex mode-3b |

### §4.3 Useful-but-noisy (consume, but rate-limit downstream)

| Wire method | Note |
|---|---|
| `item/reasoning/{summaryTextDelta, summaryPartAdded, textDelta}` | High-frequency, especially in long reasoning traces. Aggregate at the daemon, emit one EnrichedEvent per ~500ms or per item completion. |
| `item/plan/delta` *(experimental)* | Same shape concern as agentMessage/delta — buffer and coalesce. |
| `item/commandExecution/outputDelta` | Codex tool stdout/stderr per turn. Useful for streaming long shell output to chat, but per-byte; coalesce. |
| `item/fileChange/patchUpdated` | Multiple per turn for the same file; collapse on `item/completed`. |
| `turn/diff/updated` | One per file change in the turn; only the final value (at `turn/completed`) is canonical. |
| `model/rerouted` | Surface to user once per turn, even if it fires multiple times. |

### §4.4 Filter at handshake (set in `optOutNotificationMethods`)

See §4.1 list. These are either Codex-realtime (voice), Codex-cloud, Windows-only, or admin-UI noise that has no W4 consumer in ccteam.

### §4.5 Server-initiated requests — handler stubs required (§2.2)

W3b must register handlers (even if "always Decline") for at least:
- `item/commandExecution/requestApproval`
- `item/fileChange/requestApproval`
- `item/permissions/requestApproval`
- `mcpServer/elicitation/request`
- `item/tool/requestUserInput`

Without them, **any Codex turn that asks for user approval will block until Codex times out** because ccteam never replies. The mode-3b decision is: forward to existing `plan_decision` HITL flow (mirror F98), default-Decline on timeout, or auto-Accept with explicit user config. This is a W3b decision point but the current code has zero handling — see §8.

---

## §5 Mode-2 `codex exec --json` event subset (vocabulary intersection & union)

ccteam's mode-2 Codex adapter is `crates/ccteam-core/src/execution/codex_exec.rs`. It spawns `codex exec --json` and reads line-delimited JSONL events from stdout — *not* JSON-RPC, but the same payload data.

### §5.1 Naming difference

| Convention | app-server (mode 3) | `codex exec --json` (mode 2) |
|---|---|---|
| Separator | `/` (e.g. `thread/started`) | `.` (e.g. `thread.started`) |
| Discriminator field | `method` | `type` |
| Frame envelope | JSON-RPC ( `{method, params, [id]}` ) | Bare event object ( `{type, ...payload}` ) |
| Reference | `app-server-protocol/src/protocol/common.rs:1425` | `references/codex/codex-rs/exec/src/exec_events.rs:11-40` |

### §5.2 Mode-2 vocabulary (full)

Per `exec_events.rs:11-40` the `ThreadEvent` enum has exactly **8** variants:

| `type` | Payload struct | Notes |
|---|---|---|
| `thread.started` | `ThreadStartedEvent { thread_id }` | |
| `turn.started` | `TurnStartedEvent {}` | empty payload — turn id is implicit in the surrounding stream |
| `turn.completed` | `TurnCompletedEvent { usage }` | `Usage { input_tokens, cached_input_tokens, output_tokens }` |
| `turn.failed` | `TurnFailedEvent { error }` | `error: { message }` |
| `item.started` | `ItemStartedEvent { item: ThreadItem }` | |
| `item.updated` | `ItemUpdatedEvent { item }` | **Mode 2 has `item.updated`; mode 3 splits this into typed `*Delta` notifications** |
| `item.completed` | `ItemCompletedEvent { item }` | |
| `error` | `ThreadErrorEvent { message }` | |

### §5.3 ThreadItem subtypes (shared between mode 2 and mode 3)

Per `exec_events.rs:104-300` the `ThreadItemDetails` variants are: `agent_message`, `reasoning`, `command_execution`, `file_change`, `mcp_tool_call`, `web_search`, `error`, `todo_list`, `collab_tool_call`. The `protocol/v2/item.rs:212` superset adds: `Plan`, `DynamicToolCall`, `ImageView`, `ImageGeneration`, `EnteredReviewMode`, `ExitedReviewMode`, `ContextCompaction`, `HookPrompt`, `UserMessage`. Mode 2 does *not* carry the last 9 variants.

### §5.4 Intersection / union

| Capability | mode 2 (`codex exec --json`) | mode 3 (app-server) |
|---|---|---|
| Thread/turn lifecycle | ✓ | ✓ |
| Item start/complete | ✓ | ✓ |
| Mid-item deltas (text streaming) | Only via `item.updated` (single composite event with current item state) | Typed per-channel deltas (`item/agentMessage/delta`, `item/reasoning/textDelta`, `item/plan/delta`, `item/commandExecution/outputDelta`) |
| Aggregated turn diff | ✗ | ✓ `turn/diff/updated` |
| Structured plan tree | ✗ | ✓ `turn/plan/updated` (experimental) |
| Mid-turn token deltas | ✗ | ✓ `thread/tokenUsage/updated` |
| HITL approvals | ✗ (`codex exec` runs unattended) | ✓ via server requests (§2.2) |
| Model rerouted | ✗ | ✓ `model/rerouted` |
| Rate-limit visibility | ✗ | ✓ `account/rateLimits/updated` |
| Standalone command exec / process spawn | ✗ (no `command/exec`, no `process/spawn`) | ✓ |
| File-watch notifications | ✗ | ✓ `fs/changed` (with `fs/watch` subscription) |
| MCP server status | ✗ | ✓ `mcpServer/startupStatus/updated` |
| Realtime / voice | ✗ | ✓ (experimental) |
| ContextCompaction marker | ✗ (mid-thread auto-compact invisible) | ✓ as `ThreadItem::ContextCompaction` |
| Account login lifecycle | ✗ | ✓ `account/login/completed`, `account/updated` |
| Hook lifecycle | ✗ | ✓ `hook/started`, `hook/completed` |

**Take-away**: mode 2 is a strict subset of mode 3, and W3b's EnrichedEvent merger can normalise both into one canonical enum. The dot-vs-slash naming difference is the only structural distinction worth coding around.

---

## §6 W3b implications — can rmux PatternMatched substitute?

> **Principle**: Codex's typed JSON-RPC channel is **strictly richer** than what TUI rendering reveals. For nearly every notification in §2, the answer to "can we substitute by rmux PatternMatched on TUI bytes?" is **no** — substituting loses structure (decision enums, ids, breakdown tables) that the protocol gives us for free.

### §6.1 Cases where rmux PatternMatched is **redundant** (typed event is canonical)

For all of §2.1 above except the L1 process events, the UDS notification is the source of truth and any pattern match is at best a double-check. ccteam should consume the typed event, not the TUI pattern.

### §6.2 Cases where rmux PatternMatched is **legitimate complement**

| Surface | UDS event | rmux substitution | Verdict |
|---|---|---|---|
| Codex daemon crash / hang | (UDS disconnects silently — no notification fires) | rmux `process_exited` for the underlying `codex app-server` process | **Yes — only rmux can see this**. Codex won't notify its own death. |
| Codex daemon stuck (alive but unresponsive) | (none) | rmux `idle_30s` pattern on the TUI pane (no output for N seconds while a `turn/start` is outstanding) | **Yes — L1 supplement** |
| UDS reconnect race after Codex binary upgrade mid-session | (none — old socket vanishes) | rmux `pattern: "codex app-server stopping"` if Codex logs to stderr/TUI before exit | **Marginal — process exit is more reliable** |
| TUI render of a final assistant message (whole turn ended OK) | `turn/completed` notification | rmux `pattern: ">  $" prompt` after `turn/started` | **Redundant — UDS event is canonical** |

### §6.3 Cases where rmux PatternMatched is **actively wrong**

| Anti-pattern | Why it's wrong |
|---|---|
| Parsing tool args by regex on TUI bytes | TUI truncates long args; UDS `item.command_execution.command` is the full value (lossless) |
| Inferring plan steps by scanning TUI rows | TUI renders `[ ] step / [-] step / [x] step` glyphs; UDS `turn/plan/updated` gives `Vec<TurnPlanStep{status: enum}>` — much cleaner |
| Detecting rate-limit by string-match on TUI error | Worked pre-V0.8 but `account/rateLimits/updated` is now the typed source — TUI message wording can drift |
| Reading token counts off the status bar | UDS `thread/tokenUsage/updated` gives exact i64 — TUI rounds to "127k" |

### §6.4 Summary of mode-3b architecture per research §14

In mode-3b (Codex running in rmux PTY):
- **L1 (process)** — rmux owns this exclusively. `process_exited` / `idle_30s` / `SIGCHLD` are the only safety net when the UDS itself goes dark.
- **L2 (pattern)** — minimal. Only generic stuck-detection; **do not** pattern-match Codex semantics. Use UDS.
- **L3 (conversation)** — UDS notifications (`item/agentMessage/delta`, `item/completed`, `item/reasoning/*`). Don't read the TUI.
- **L4 (semantic)** — UDS exclusively (plan tree, tool args, decisions, status flags). Mode-3b is "rmux PTY hosts the TUI; the daemon's UDS bridge is the truth".

---

## §7 Authentication / connection edge cases

### §7.1 Authentication

Codex itself authenticates to OpenAI; ccteam authenticates to Codex via the local UDS only (no token exchange on the JSON-RPC channel). However, Codex's *upstream* auth state surfaces in three places ccteam should watch:

| Surface | Where it leaks | Today's handling |
|---|---|---|
| `account/updated` notification | `auth_mode: ApiKey \| Chatgpt \| ChatgptAuthTokens \| AgentIdentity`, `plan_type` | ccteam doesn't subscribe |
| `account/login/completed` notification | `success: bool`, `error?` | ccteam doesn't subscribe |
| `error` notification with `will_retry: false` for auth failures | TurnError carries `error.type` discriminating auth from rate-limit | ccteam treats all `error` (translated as `turn/failed`) uniformly |
| `getAuthStatus` / `account/read` request | Polling-style auth check | `crates/ccteam-cli/src/commands.rs::doctor` should call this; not verified |

**Edge case**: If Codex's ChatGPT auth tokens expire mid-session, the server may send the `account/chatgptAuthTokens/refresh` **server-initiated request** (§2.2). Without a handler, the next turn fails. W3b should at minimum log + auto-Decline this request, with a clear "Codex auth needs refresh" error surfaced to the user.

### §7.2 Connection handshake

| Step | Server expectation | ccteam today |
|---|---|---|
| TCP/UDS connect | OK | ✓ |
| `initialize` request | **Required before any other method** — server starts buffering notifications and gates feature flags | **MISSING** (`codex_app_server.rs::start_thread` calls `thread/start` directly). See §8. |
| `Initialized` notification | Signals the client is ready to receive server-initiated requests | **MISSING** |
| Any thread/turn method | OK | ✓ |

**Without the initialize handshake**:
- `experimental_api` defaults to `false` → ~30% of the notification surface in §2 is silently filtered out by the server.
- `request_attestation` defaults to `false` → no attestation requests (harmless).
- `opt_out_notification_methods` is empty → ccteam receives all stable + non-experimental notifications, which is what saves the current implementation from being broken on the *consume* side. But it pays for it in bytes-on-wire for surfaces ccteam doesn't use (realtime, Windows, fs/watch).

### §7.3 Codex binary upgrade mid-session

Codex's daemon owns the UDS. If the user upgrades `codex` binary:
1. The running daemon keeps serving until it's restarted (no signal in the protocol).
2. `ccteam doctor --check-codex-auth` and ad-hoc `codex` CLI invocations may use the *new* binary against the *old* daemon — schema/version drift can produce parse errors.
3. The new daemon, when started, binds the same UDS path — old connections see EOF.

**Mitigation**: ccteam should treat UDS `EOF` as a transient failure → reset cached client (already done at `codex_app_server.rs::forget_client`), retry `initialize`+`thread/resume(thread_id)`. Currently the adapter does **not** auto-reconnect — the orchestrator's `progress.jsonl` poller is the fallback, which is the V0.6.1 Wave-3 D9 retained risk.

### §7.4 Reconnect / resubscribe

`thread/resume` is the documented re-entry for an existing thread (mode 3 chat lossless). Notifications for that thread will replay (Codex caches the recent backlog). The bridge's idempotency guard (`is_terminal_progress` at `codex_app_server.rs:637`) is the only thing preventing double-counted `turn/completed` writes on reconnect — verify behaviour under W3b.

---

## §8 Open questions

1. **Missing `initialize` handshake in ccteam** (§7.2). The doc-comment at `crates/ccteam-core/src/execution/codex_app_server.rs:10` claims `"initialize (if needed) → thread/start"` but the implementation skips it. Is this an intentional shortcut (Codex's `app-server` tolerates being called without initialize in some versions) or an unnoticed regression? **Verify against the Codex version users actually run.** W3b should either send the handshake (recommended) or document why it's not required.

2. **Dead `"item/updated"` branch** at `codex_app_server.rs:525-527`. The mode-3 protocol has no `item/updated` notification — it splits state changes into typed `*Delta` notifications + `item/completed`. This match arm is unreachable for mode 3 but matches mode-2 wire shape (`item.updated`). Likely a copy-paste artefact from when mode-2 / mode-3 translation shared a function. Recommended: remove or repurpose to handle one of the `*Delta` channels.

3. **No HITL server-request handlers** (§2.2 + §4.5). Any Codex turn that triggers `item/commandExecution/requestApproval` or sibling approvals currently stalls — Codex waits forever for ccteam's reply. Today this is masked because ccteam uses Codex with relaxed sandbox profiles that don't trigger approval flows, but W3b mode-3b will run user-facing Codex turns; this is a blocker.

4. **`"error"` notification vs `turn/failed`**. There is no `turn/failed` notification in the protocol — `error` is the wire name, and `turn_id` + `thread_id` are payload fields. The current `codex_app_server.rs::translate_notification` matches `"turn/failed"` (line 504), which is dead code. The "error" branch (which would match `error`) is missing — terminal failures are translated only via `turn/completed` (when usage is zero) or as the generic skip-unknown path. Verify with a real Codex run that turn failures actually surface in ccteam.

5. **`thread/status/changed` is unmatched.** This is the canonical HITL detection signal but ccteam never reads it. Was this an oversight, or intentional because the orchestrator's `progress.jsonl` poller covers it? Recommend bridging it to a new `progress.jsonl` event `chat_thread_status` so the poller doesn't have to call `thread/read` repeatedly.

6. **`thread/tokenUsage/updated` enables mid-turn budget caps.** Currently `compute_cost_summary` aggregates only at `turn/completed`. With this notification, ccteam could trip the budget cap mid-turn and call `turn/interrupt` to cut off a runaway turn. Decide W3b scope: (a) just record the deltas, or (b) wire mid-turn interrupt.

7. **`turn/diff/updated` semantics**. Is the `diff: String` field the *current* aggregated diff at the moment of emission, or a delta since the prior emission? §2 lists it as "aggregated", consistent with the doc-comment at `protocol/v2/turn.rs:337-343`. W3b should treat it as idempotent-replace, not append. Confirm against an actual Codex run.

8. **`process/exited` vs `command/exec` final response timing.** `command/exec`'s response is deferred until the process exits AND all `outputDelta` notifications have been sent (per `protocol/v2/command_exec.rs:25-27`). For `process/spawn`, the `process/exited` notification is the canonical end. ccteam doesn't use either today, but if W4 wants to drive Codex *as a tool* (mode-3b orchestrator invokes `process/spawn` for sub-tasks instead of mode-2 `codex exec`), this ordering matters.

9. **Server `optOutNotificationMethods` validation.** §4.1 recommends a deny-list at handshake. Verify the server enforces it (`InitializeCapabilities::opt_out_notification_methods` per `v1.rs:54`) — search of `app-server/src/` shows `opted_out_notification_methods` is read in `lib.rs:144,741,...` and consulted in `outgoing_message.rs` but the actual filter point should be confirmed by `tracing::trace!` instrumentation, not just a "yes the field exists" claim.

10. **Future Codex protocol drift.** The protocol's `EXPERIMENTAL_CLIENT_METHODS` const + `#[experimental(...)]` annotations are stable enough that ccteam's "forward-compat warn + skip" pattern (`vendor_compat::warn_unknown_vendor_token`) handles unknown methods gracefully. But a wholesale rename (e.g. `turn/completed` → `turn/finished`) would silently drop critical events. W3b should consider a snapshot-test: serialise the `EXPERIMENTAL_CLIENT_METHODS` const + the `server_notification_definitions!` wire names against an expected baseline (regenerable). The protocol exports `generate_json_with_experimental` / `generate_ts_with_options` (`app-server-protocol/src/lib.rs:11-13`) — these can power a CI guard.

11. **`item/started` started_at_ms / `item/completed` completed_at_ms ordering across reconnect.** On `thread/resume`, Codex may replay item notifications. Are the `_ms` timestamps original (allowing chronological ordering) or replay-time? Verify against the rollout store (`references/codex/codex-rs/rollout/`).

12. **`thread/realtime/*` deserves a deferred decision.** The 8 realtime notifications are all experimental + voice/audio. ccteam will likely never enable them, but if `experimentalApi: true` is set at handshake they'll arrive on the wire. Recommended: opt out by name (§4.4) so the server-side filter saves the bytes, not the client-side filter.

---

## Appendix A — File index for further reading

| Concern | Path |
|---|---|
| Method registry (client→server requests) | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:434-1033` |
| Method registry (server→client requests) | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1277-1341` |
| Method registry (server notifications) | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1425-1517` |
| Method registry (client notifications) | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1519-1521` |
| Initialize handshake types | `references/codex/codex-rs/app-server-protocol/src/protocol/v1.rs:26-70` |
| `ThreadItem` subtype enum (mode 3) | `references/codex/codex-rs/app-server-protocol/src/protocol/v2/item.rs:208-393` |
| `ThreadEvent` (mode 2) | `references/codex/codex-rs/exec/src/exec_events.rs:11-40` |
| Turn-level notification payloads | `references/codex/codex-rs/app-server-protocol/src/protocol/v2/turn.rs` |
| Thread-level notification payloads | `references/codex/codex-rs/app-server-protocol/src/protocol/v2/thread.rs:1118-1187` |
| ccteam mode-3 codex adapter | `crates/ccteam-core/src/execution/codex_app_server.rs` |
| ccteam mode-3 JSON-RPC client | `crates/ccteam-core/src/execution/codex_jsonrpc.rs` |
| ccteam mode-2 codex adapter | `crates/ccteam-core/src/execution/codex_exec.rs` |
| ccteam progress.jsonl write paths | `crates/ccteam-core/src/progress.rs`, `crates/ccteam-core/src/orchestrator.rs` |
| Forward-compat seam | `crates/ccteam-core/src/vendor_compat.rs` (`warn_unknown_vendor_token`) |
| Research context | `docs/research/embedded-mux-unified-architecture.md` §13, §14, §15 |

---

*Catalog last verified against Codex commit `76845d716b` (2026-05-10). Re-run §A index commands after any `references/codex/` bump.*
