# v0.8.7 fix.md 批次(FIX-1/2/3 实机 bug)Handoff

> 配 `fix.md`(用户实机反馈 + file:line)。直接 dev、无 PR、commit `v0.8.7:` 前缀。
> **注**:fix.md 行号校准于 b4a6076(W2 前),W2 改动了 main.rs/gateway.rs → 实现按 symbol grep 落地。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1919/0**(W2 后基线 1912 +7;另 web vitest 2 例)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `doctor --verify-mcp` 17/17、0 drift · `cargo build -p ccteam-web` OK(SPA 重建)。

## FIX-1 · 出站文件发到 live session
- **Decided**:新 `pub Gateway::reply_target_for(project, role) -> Option<(channel, chat_id)>`(镜像 `session_sid_for` 的 (project,role) dedup find,再经 `pump_target` 的 `reply_to→owner` 解析;只读、无 .await)。`gateway: Option<&GatewayHandle>` 穿 `execute_chat_send_file → run_chat_send_file → build_send_file_event` + `execute_interaction_ask`。新 async helper `resolve_live_reply_target`:**锁内只解析、return 前 drop guard,再做 fs/send**(镜像 `run_session_collect` lock-discipline)。`build_send_file_event` = `live_target.or_else(resolve_home_chat)` → live session 优先、registry 兜底。
- **Rejected**:不把 `build_send_file_event` 改 async(保持 sync 可单测;live target 在 async caller 解析后注入)。registry 不删(仍管 inactive bot + @handle)。

## FIX-2 · web 默认 role `assistant`→`cto` + create 路径 persona 校验
- **Decided**:(a) SPA `DEFAULT_ROLE='cto'` + `ROLE_SUGGESTIONS`(cto 打头)抽到**无依赖** `crates/ccteam-web/web/src/pages/chatDefaults.ts`(让 vitest 在 node env 直接 import,不拖 ChatConsole 的 window 链);ChatConsole `effectiveRole = role.trim()||DEFAULT_ROLE`。(b) 新 `ensure_role_exists(cwd,role)` 放**单一 create chokepoint** `Gateway::start_session`(web `create_session_api`/IM `/new`/cto `run_session_spawn` 全经此)——在 `next_session++`/spawn **之前**,role 无 `.claude/agents/<role>.md` → 清晰双语错误(指 `/role` 或 `ccteam role add`),不留死 pane。**豁免**:`.claude/agents/` 不存在(未 init / 测试裸目录)→ skip 校验(零 churn 既有 ~44 create-path 测试;prod 必有该 dir)。
- **Rejected**:不用 process-global 校验开关 env(需散设 ~9 测试文件 + 是掩盖 prod bug 的后门);用 agents-dir-exists 启发式更干净零 churn。
- **未实证(诚实)**:`claude --agent <未定义>` 确切 vendor 行为未测 —— 但 persona 校验令其 moot(永不 spawn 未定义 agent)。

## FIX-3 · `ccteam status` STUCK 误报
- **Decided**:选 fix.md option (b)。`summary_from_state(paths, state)` 改从 **progress.jsonl 末行 ts**(新 helper `last_progress_event_ts` 用既有 `progress::last_event`)取 stall 基线,并**回填** `state.last_progress_event_at` → CLI/web「last event」标签也在同一读侧点修好。刚活跃项目不再 STUCK;真停 ≥15min 仍 STUCK。
- **Rejected**:option (a)(在 `append_event` bump state)—— `append_event` 在 ccteam-harness,**不能依赖 ccteam-core**(cargo cycle),写 state.json 破层级。不整删 `last_progress_event_at` 字段(ccteam-web/cli 仍读它做 label;回填更省)。

## Risks
- FIX-2 agents-dir-exists 启发式:若 prod 项目无 `.claude/agents/`(从未 init / 用户删),create 会 skip 校验、仍可能 spawn 未定义 agent。已 mitigated(start_session 已要求项目注册 + init 必种 dir)+ doc 注释。
- FIX-3 每次 `ccteam status`/dashboard 多读各项目 progress.jsonl 末行一次(可忽略)。
- 5 个 ccteam-web `ws_*` PTY 测试本 sandbox 失败(pipe-pane 不能流,**pre-existing 环境**,CLAUDE.md 记;CI/non-WSL 复测)。
- 已知 flake `hook_sh_with_action_routes_kind_and_action_to_cli`(.spawn().unwrap() 并发 ETXTBSY)本批未触发,与本改无关。

## Files
- ccteam-im:`gateway.rs`(`reply_target_for` + `ensure_role_exists` + 4 测试)。
- ccteam-cli:`main.rs`(file-send/ask 穿 gateway + `resolve_live_reply_target` + 测试)。
- ccteam-core:`queries.rs`(`summary_from_state` stall 改 progress.jsonl + 2 测试)、`lib.rs`(`agents_dir` re-export)。
- ccteam-web:`web/src/pages/{ChatConsole.tsx, chatDefaults.ts(新), ChatConsole.test.ts(新 vitest)}`。

## Remaining
- 可选:`stall::silent_seconds` + watchdog.rs 的旧 `last_progress_event_at` 写路径仍是旧模型(仅 re-export、无 prod caller 命中 buggy 路径);若该字段真退役可后续清理。
- 可选 FIX-2 step3(web 送空 role 时后端默认 cto)未做:SPA 总送显式 role、空 role 仍 400,SPA 不可达;persona 校验 + SPA 默认已全覆盖报告的 bug。非 SPA client 需要再加。
- **版本 bump 0.8.7 + CLAUDE.md §一 baseline + tier-1 doc sync = W6 ship-gate**(含 MCP 12→17 文案、STUCK 行为说明)。
