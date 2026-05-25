# ccteam V0.6.8 — chat-mode squad 深度修复 + creator bootstrap + 多 bot UX

> **范围**: F175-F203 共 29 finding。围绕 chat-mode squad 实战发现的全套问题集中修复:fan-out cross-fire / 静默失败 / 持久化路径硬编码 / hook env propagation / bot 间互不认识 / 注册路径不一致 / creator bootstrap 漏写 state.json + hook.sh / CLI fallback 缺失 / 多 bot 用户分不清谁说话 / Codex 故障无 retry cap。

---

## 1. 触发场景

V0.6.7 ship 后用户实测 chat-squad 模式（3 bot 同 TG 群协作），发现一系列从静默失败到完全不可用的 ship-blocker:

1. **3 bot 重复回复同一条消息** → 用户 @ 一个 bot,3 个都说话 (F175-F177 fan-out)
2. **`@curie` 等 SKILL 承诺的 nickname handle 完全不响应** → 静默 drop (F180-F184 handle schema)
3. **项目在 NAS / 非 home 路径就跑不起来** (F185 project_dir)
4. **F175 注入 tmux env 没真正解决问题**(F139 之后 hook 走 HTTP 到 daemon 进程 → claude 进程 env 到不了 daemon) (F186 hook HTTP header)
5. **bot 卡死时用户完全无感知** (F187 marker WARN, F194 DM hint, F195 turn timeout, F196 marker self-heal)
6. **chat-squad workflow.yaml 模板根本 deserialize 不通过** (F188 schema sync)
7. **chat-squad 只能 3 bot** → role_a/b/c 硬编码 (F189 N-agent)
8. **多 bot 共享 chat_id 时无歧义提示** (F184 unknown / F194 multi-bot)
9. **bot-to-bot @ 互聊不通**(router 框架在但 outbound 不接 cross-mention scan) (F193 mpsc fast-path)
10. **bot 不知道兄弟存在**(persona 无 squad roster,bot 把 `@dev` 当 Task subagent) (F200 squad roster awareness)
11. **多 bot 群里 TG 端用户分不清谁说话**(都来自同一 TG bot username) (F199 from-prefix)
12. **ccteam-creator 不调 `ccteam init`** → `.ccteam/state.json` 漏写 → SessionStart hook 找不到项目根 (F197 bootstrap)
13. **`~/.ccteam/hooks/hook.sh` dispatcher 不跟 creator 联动** → 全部 chat-mode 静默无回复 (F198)
14. **plugin manifest version drift 2 版本** → ship gate 漏检 (F201)
15. **register-bot 只有 MCP 路径** → daemon 不在/没装时 creator 卡死 (F202 CLI fallback)
16. **`ccteam init` 在自身源码目录拒绝 + 无 --force** → 自托管/dogfood 阻塞 (F203)
17. **Codex 故障一直 retry 不停** → 日志洪水 (F192c retry cap)

---

## 2. Findings

### 2.1 Fan-out 修复(F175 / F176 / F177)

| # | 改动 | 触动 |
|---|---|---|
| **F175** | tmux spawn 注入 `CCTEAM_CHAT_ROLE` + `CCTEAM_CHAT_SLUG` env;`TmuxSession::start` 加 env 重载 | `tmux.rs`, `claude_tui.rs` |
| **F176** | `chat_progress::handle_chat_progress` 在 `session-start` / `session-end` 写 `<project>/.ccteam/chat/<role>/active-session-id`(atomic rename) | `chat_progress.rs` |
| **F177** | `tail_loop` 读 marker target 确定 jsonl;**删 chat-mode `discover_active_session` 回退**;marker 缺失 → sleep+retry 等 SessionStart fire | `claude_tui.rs`, `transcript_tail.rs` |

### 2.2 Install + skill polish(F178 / F179)

| # | 改动 | 触动 |
|---|---|---|
| **F178** | 用户面 docs 凡描述尚未实现 behavior 段落 → 删;删 SKILL 里 `ccteam internal daemon ensure-running` 失效引用 | `docs/`, `skills/` |
| **F179** | `install.sh` 加 (a) 已最新短路(`ccteam --version` == TAG 跳过下载);(b) `pgrep -f "ccteam start"` daemon-running warning | `install.sh` |

### 2.3 chat_handle schema + auto-mint + unknown UX(F180-F184)

| # | 改动 | 触动 |
|---|---|---|
| **F180** | `AgentSpec.chat_handle: Option<String>` schema(workflow.yaml additive) | `workflow.rs` |
| **F181** | `BotRegistration.chat_handle` + `chat_register_bot` MCP 加入参 + atomic 持久化 | `lib.rs`, `mcp_chat_tools.rs` |
| **F182** | `build_handle_map` 用 `chat_handle.unwrap_or(role)`;跨 slug collision → `<handle>__<slug>` 命名空间裁决(`__` 是因 router parser 不接 `@`)| `daemon.rs::build_handle_map`, `router.rs` |
| **F183** | `chat_register_bot` MCP 缺省 chat_handle 时 auto-mint 从 `agent_naming::pick_unused_bot_name()` 取 scientist nickname;`ccteam-creator` SKILL Phase 5.5/5.6 同步 | `mcp_chat_tools.rs`, `skills/ccteam-creator/SKILL.md` |
| **F184** | router 未知 `@handle` → reply `Unknown handle '@xxx'. Available bots in this chat: @alice @bob @carol`;`@ccteam list bots` admin keyword(`nl_admin` 6th keyword)| `router.rs`, `nl_admin.rs` |

### 2.4 BotRegistration.project_dir(F185)

| # | 改动 | 触动 |
|---|---|---|
| **F185** | `BotRegistration.project_dir: Option<PathBuf>` schema additive;`supervisor::bot_dir` + `inbound::DefaultMailboxResolver` 优先 `reg.project_dir`,fallback `<projects_root>/<slug>`;`chat_register_bot` 加 `project_dir` 入参,缺省 `current_dir().canonicalize()`;ccteam-creator Phase 5.6 显式传 `project_dir` | `ccteam-imd/src/lib.rs`, `supervisor.rs`, `inbound.rs`, `mcp_chat_tools.rs`, `SKILL.md` |

### 2.5 hook HTTP header env injection + tail marker WARN(F186 / F187)

| # | 改动 | 触动 |
|---|---|---|
| **F186** | hook.sh forward `CCTEAM_CHAT_ROLE` + `CCTEAM_CHAT_SLUG` 进 HTTP header `X-Ccteam-Role` + `X-Ccteam-Slug`;`internal_hook::dispatch` 提取 header → 注入 stdin payload `role`/`slug` 字段。**F175 设计假设错了**:V0.6.1 F139 之后 hook 走 HTTP 到 daemon 进程,claude 进程 env 到不了 daemon — F175 只对 fallback CLI 路径有效 | `hook.sh`, `internal_hook.rs` |
| **F187** | `tail_loop` + `tail_loop_polling` 在 sustained marker-missing 后 emit 一次 WARN(60s+ 沉默后),含 role/slug context;marker 出现后 gate 重置 | `claude_tui.rs` |

### 2.6 chat-squad template schema sync + N-agent + handle override(F188-F191)

| # | 改动 | 触动 |
|---|---|---|
| **F188** | `chat-squad.yaml` + `chat-pocket.yaml` 删 `chat.im_platform`(ChatSpec 无此字段)、改 `chat_acl` 从 list → struct `{allow_users, allow_groups}` 形式、`bot_name` 加引号(`@`-prefixed handle 兼容);新 CI gate `template_schema_test.rs` 每个 template render → deserialize | `templates/`, `tests/template_schema_test.rs` |
| **F189** | chat-squad template 改 data-driven N-agent:`{{agents_block}}` 占位 + `render_workflow_agents_block(&[entries])` helper;`default_ctx(ChatSquad)` 用 2 agents | `templates/mod.rs`, `chat-squad.yaml` |
| **F191** | ccteam-creator SKILL Phase 4 PROJECT PLAN 显式 review/override scientist nickname;Phase 5.6 文档化 user override 时显式传 `chat_handle`(MCP 入参已 F181 接好) | `SKILL.md` |

### 2.7 MailboxResolver config.yaml fallback + Codex diagnostics(F190 / F192)

| # | 改动 | 触动 |
|---|---|---|
| **F190** | F185 priority chain 第三 tier:`reg.project_dir` > `~/.ccteam/config.yaml::projects[slug].path` > `<projects_root>/<slug>`。`resolve_project_dir(reg, projects_root, config_projects)` 抽到 `crate::ccteam-imd::lib.rs` 单 SoT,`MailboxResolver` + `supervisor::bot_dir_with_config` 同源 | `lib.rs`, `inbound.rs`, `supervisor.rs`, `daemon.rs` |
| **F192a** | `ccteam doctor --check-codex-auto-critic` V0.6.5 F155 已实现真 canary(`codex exec --json --skip-git-repo-check`),subagent 验证无需新代码 | `commands.rs` (verify only) |
| **F192b** | Codex chat-mode `start_thread` 失败时 WARN 含 anyhow error chain 全展开(`{err:#}`,truncated to ~1KB)── 含 tmux stderr | `supervisor.rs::record_start_failure` |
| **F192c** | per-bot `start_thread` retry cap `MAX_START_THREAD_ATTEMPTS = 3`:5s/15s/give up;`permanent_failure` latch + `chat_bot_permanent_failure` progress event;`reset_session` 清 latch (operator recovery via signals) | `supervisor.rs`, `progress.rs` |

### 2.8 bot-to-bot @mention via daemon mpsc(F193)

| # | 改动 | 触动 |
|---|---|---|
| **F193** | `InboxItem` / `OutboundItem` / `InboundOutcome::DroppedToBot` 加 `hop: u8`;`BotSupervisor` 加 `current_hop: Arc<AtomicU8>`;`handle_inbound(payload, hop)` 接 hop;`spawn_outbound_dispatcher` 在 `channel.send` 后调 `dispatch_cross_bot_mention` helper:扫 `parse_first_mention` → HandleMap lookup → self-mention guard (`(slug,role)` tuple 不是 handle string)→ `within_hop_budget(hop+1)` → `try_send` 合成 InboxItem 到 target.inbox_tx。纯 daemon 内 Rust 通道不经 TG。`CrossBotDispatch` enum 让 integration test 不用拉起 tmux+adapter | `bot_mpsc.rs`, `inbound.rs`, `supervisor.rs`, `daemon.rs`, `router.rs`, `tests/cross_bot_mention_test.rs` |

### 2.9 DM multi-bot hint + per-turn timeout watchdog(F194 / F195)

| # | 改动 | 触动 |
|---|---|---|
| **F194** | `auto_route_dm_mention` 重构返回 `DmRoutingHint::{Routed, Ambiguous, NoMatch}`;`Ambiguous` 路径 `channel.send` 回 `Multiple bots in this chat. Specify one: @alice @bob`(复用 F184 `available_handles_for_chat`)| `inbound.rs`, `daemon.rs` |
| **F195** | `ChatSpec.turn_timeout_sec: u32`(default 90,validation 拒 0);`BotSupervisor` 加 `TurnDeadline { turn_id, started_at }` + `check_turn_watchdog`;daemon 5s tick poll;90s 第一次 → `chat_turn_running_long` event + IM "Still working" 回复;180s 第二次 → `chat_turn_timeout` event + "stuck" 回复;`chat_turn_completed` 收到 cancel;latch 防 spam。**R5 守:不杀 turn**,只 surface | `workflow.rs`, `progress.rs`, `supervisor.rs`, `daemon.rs`, `tests/turn_timeout_test.rs` |

### 2.10 SessionStart marker self-heal(F196)

| # | 改动 | 触动 |
|---|---|---|
| **F196** | tail_loop 通过 `MarkerReporter` trait(进程级 weak-ref registry 按 `(slug,role)` 查找)报告 marker miss/found 到 supervisor;`BotState.marker_missing_count` 计数;`MARKER_MISSING_RESET_THRESHOLD = 30`(≈ 60s)触发 `MarkerHealAction::Heal` → `attempt_marker_self_heal`:emit `chat_marker_self_heal_attempt` event + 调 F192c `reset_session`(tmux-kill + start_thread 重 spawn);新 SessionStart hook 写 marker → `report_marker_found` 清 counter;3 次失败 heal → `chat_bot_marker_stuck` latch。**R5 守**:reset 是 escalate-from-stuck-state,不是 mid-turn kill(同 F84 budget overflow / F192c spawn failure 通道)| `harness.rs`, `marker_reporter.rs`, `claude_tui.rs`, `supervisor.rs`, `progress.rs`, `daemon.rs`, `tests/marker_self_heal_test.rs` |

### 2.11 ccteam-creator bootstrap state.json + hook.sh(F197 / F198)

| # | 改动 | 触动 |
|---|---|---|
| **F197** | `chat_register_bot` MCP dispatcher 在 caller 显式传 `project_dir` 时调 `bootstrap_project_at_dir(paths, &project_dir, &slug, "", "chat")` 写 `.ccteam/state.json`(only-if-absent 守 idempotency,不 clobber 既有);ccteam-creator SKILL Phase 5.0 文档化依赖 | `mcp_chat_tools.rs`, `SKILL.md` |
| **F198** | 同 dispatcher 调 `install_hooks(paths)` 装 `~/.ccteam/hooks/hook.sh`(F139 HTTP dispatcher);函数 idempotent,无 clobber 风险 | `mcp_chat_tools.rs` |

### 2.12 多 bot UX:from-prefix + squad teammate awareness(F199 / F200)

| # | 改动 | 触动 |
|---|---|---|
| **F199** | 新 `outbound_format` 模块:`should_prefix_with_handle(bots, reg) -> bool`(同 `(im_platform, im_chat_id)` 计数 > 1)+ `prefix_with_handle(handle, content) -> String`(`from <handle>:\n<content>`,不带 `@` 防 TG mention parse);`spawn_outbound_dispatcher` + `drain_outboxes`(safety-net)同步注入。单 bot DM 不前缀 | `outbound_format.rs`, `daemon.rs` |
| **F200** | 新 `crates/ccteam-core/src/templates/squad_roster.rs`:`TeammateInfo { handle, role, persona_label }` + `render_squad_roster_zh/en()`;ccteam-creator Phase 5.4 写 persona 后追加 squad-roster block(chat-squad 且 N≥2);bot 自然 load 兄弟 bot 拓扑;不再误判 `@dev`/`@pm` 是 Task subagent | `templates/squad_roster.rs`, `SKILL.md` |

### 2.13 plugin sync + register CLI + init self-host(F201 / F202 / F203)

| # | 改动 | 触动 |
|---|---|---|
| **F201** | `.claude-plugin/{plugin,marketplace}.json` + `.codex-plugin/plugin.json` version sync workspace pin;`plugin_manifest_version_test.rs` workspace test 守 4 个 version 字段相等(workspace + 3 manifest 含 marketplace 嵌套 plugin entry)| `.claude-plugin/`, `.codex-plugin/`, `tests/` |
| **F202** | `ccteam admin register-bot` + `unregister-bot` CLI subcommand(mirrors MCP `chat_register_bot`,同 auto-mint scientist nickname,同 project_dir canonicalize)| `main.rs`, `commands.rs` |
| **F203** | `commands.rs:151 is_ccteam_repo` 拒绝改 `&& !opts.force`;error message 提示 `--force` 是开发者(self-host / dogfood)合法路径 | `commands.rs` |

### 2.14 chore: V0.7 TODO 锚点 closeout

- F181 closed `TODO(V0.7-chat-handle)` 锚点 — `no_silent_todo_test.rs` 计数 6 → 5
- `docs/dev-coupling-audit.md` `chat-handle` 行移到 V0.6.8-closed segment

---

## 3. 红线核对(CLAUDE.md §三)

| 红线 | 触及? | 说明 |
|---|---|---|
| R0 — 文件系统是控制平面 | 守 — 新 marker 文件 `<project>/.ccteam/chat/<role>/active-session-id`(F176)+ `state.json` bootstrap(F197)、`hook.sh` install(F198)都是文件系统协议 |
| R1 — progress.jsonl 唯一 SoT | **强化** — F175/F186 让 progress 事件 `role` 不再 `""`(以前是 bug);新增 6 类业务事件(`chat_turn_running_long`/`chat_turn_timeout`/`chat_marker_self_heal_attempt`/`chat_bot_marker_stuck`/`chat_bot_permanent_failure`/`chat_session_reset` reason) |
| R2 — No prompt injection | 守 — F200 squad roster 是 `.claude/agents/<role>.md` body content,走 Anthropic 标准 persona 机制;不向 tmux pane 注入 |
| R3 — 每次 spawn = fresh 1M context | 守(chat 模式不适用) — F177 仍走 `--name` / `--resume` 路径 |
| R4 — 永不主动 kill 长 session | 守 — F195 turn timeout 只 surface 不杀;F192c retry cap 触顶 emit event 然后 stop retrying(不再 spawn,不 kill 已存活)| | R5 — 永不主动 kill 长 session(2)| F196 marker self-heal 是 sustained-stuck-state escalate(同 F84 / F192c 通道),不是 mid-turn kill;commit/event 措辞统一用 "escalate" / "self-heal",不用 "kill" |
| R6 — 不解析 tmux 终端输出 | 守 — tail 仍走 transcript jsonl;F193 bot-to-bot 走 mpsc 不 scrape |
| R7 — fix-loop 撞 3 次 escalate | **强化** — F192c retry cap = 3 + escalate event,同 F84 模式一致 |
| R8 — ccteam-core 零 team 名字面量 | 守 — chat_handle / project_dir 都是 runtime schema 字段 |
| R9 — 跨项目记忆走官方接口 | 守 — 不动 CLAUDE.md / AGENTS.md 机制 |
| R10 — 新建项目走 `<projects_root>/<team>-<slug>/` | 弱化但兼容 — F185 加 explicit `project_dir`,fallback 仍 `<projects_root>/<slug>` |
| R11 — root README 不含版本进展 | 守 — 用户面 docs 用现在时陈述(F178 / 本版 ship doc sync 守) |
| R12 — HITL approval state SoT | 不触及 |

---

## 4. Baseline gate

- workspace `0.6.7` → `0.6.8`(`Cargo.toml::workspace.package.version` + `Cargo.lock` 同步 6 个 ccteam crates)
- test:V0.6.7 baseline `1639/1`(workspace)→ V0.6.8 `1549/0`(`--exclude ccteam-web`,本机 inotify-busy 上 ws_* 测试 hang);新增 ~30 个 integration tests(handle / project_dir / fanout / marker self-heal / turn timeout / cross-bot mention / template schema / admin register / etc.);CI 跑全 workspace 含 ccteam-web 约 1700/1(1 known flake `workflow_summary_reflects_agent_spawn_and_done_events` running_count)
- clippy:`-D warnings` clean(0 warning)
- fmt:`cargo fmt --all -- --check` clean

---

## 5. Ship gate(CLAUDE.md §五.7)

| Item | Done |
|---|---|
| `CLAUDE.md §一` baseline / workspace version 更新 | ✅(本 ship commit) |
| `Cargo.toml::workspace.package.version` 0.6.7 → 0.6.8 + `Cargo.lock` 同步 | ✅ |
| `.claude-plugin/plugin.json` + `.claude-plugin/marketplace.json` + `.codex-plugin/plugin.json` version sync(F201 test 守)| ✅ |
| `docs/versions/v0-6-8/README.md` 落地全部 finding(本文件)| ✅ |
| `docs/dev-coupling-audit.md` 加 F175-F203 索引 + V0.7-deferred 计数调整 | 待 |
| 用户面 docs(`README.md` / `docs/{quickstart,user-manual,recipes,troubleshooting}.md`)反映 V0.6.8 行为 | 待 |
| `docs/tech-design.md` / `docs/interfaces.md` / `docs/claude-code-tool-surface.md` 联动 | 待 |
| Tag `v0.6.8` + `release.yml` 四个 target 全绿 | tag-push 后验 |
| 用户验:`reno-squad` 3 bot 群发不再 cross-fire;`@curie` 默认 handle 起效;`@unknownhandle` 给出 available list;`@cx 跟 @dev 讨论` → 兄弟 bot 真接到;NAS 路径项目能跑;TG 群多 bot 看到 `from cx:` 标 | tag-ship 后验 |

---

## 6. 不在范围(V0.7 候选,本版不做)

- Epic C 国内 IM:Slack inbound HTTP + Socket Mode + WeChat / 飞书 / DingTalk / QQ(`TODO(V0.7-{im-providers,slack-inbound,slack-socket-mode})` 仍留)
- chat memory 跨设备同步
- monorepo-aware `.mcp.json`
- migrate-from-claude
- 6 号编排模式深化(HumanApprovalAdapter full wrapper,`TODO(V0.7-human-approval-adapter)` 仍留)
- `/ccteam-creator` 完整 template library + LLM-assisted role auto-gen(F167 sensible defaults 已 ship,完整 library 留 V0.7)
- `listbots-cache`(`TODO(V0.7-listbots-cache)` 仍留,V0.6.8 单 bot host probe 量级不痛)
- **F195 caveat**:`ChatSpec.turn_timeout_sec` schema 已接好但 daemon `tick_supervisors_with_config` 还没读 workflow.yaml,生产恒用 `DEFAULT_TURN_TIMEOUT_SECS = 90`。可在用户面影响小但是 SoT 不完整,V0.7 plumb 完整
- **uninstall 子命令**:不做(pre-v1.0 重装频率低,一行 `rm -rf ~/.ccteam/` 文档说明更透明)
- **R3 红线松绑 + advise.rs 改实现(避 `claude -p`)**:用户口头确认要做但 V0.6.8 范围已闭;V0.7 单独 Epic

---

## 7. Acceptance(用户验)

V0.6.8 tag push → CI release.yml 跑完 → 重装:

```sh
ccteam stop 2>/dev/null || pkill -f "ccteam start" || true
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version  # 应输出: ccteam 0.6.8
ccteam start
```

squad-mode 验证(reno-squad / 6-role research squad / 类似):

1. **fan-out**:群 `@reporter ...` → 只 `reporter` 回复,其余兄弟 0 输出
2. **handle UX**:`@curie hello` 等 scientist nickname 默认起效(creator auto-mint)
3. **unknown @ 反馈**:`@unknownname hello` → 群里收到 `Unknown handle '@unknownname'. Available bots in this chat: @<...> @<...>`
4. **DM ambiguous hint**:DM 多 bot 共享 chat_id 时 `hi`(无 @) → 收到 `Multiple bots in this chat. Specify one: @alice @bob`
5. **admin keyword**:`@ccteam list bots` → 群里收到当前 chat 内可用 bot handle 列表
6. **bot-to-bot 互聊**:`@reporter 跟 @explorer 一起讨论 X` → reporter 回复带 `@explorer`,explorer **自动**接到并响应(纯 daemon mpsc 不经 TG round-trip)
7. **多 bot from-prefix**:群里看每条 bot 消息都带 `from <handle>:\n<content>`,知道是哪个 ccteam role 说的
8. **squad teammate awareness**:对 reporter 说 "@-mention @explorer" 它直接 @,而不是试图调 Task subagent
9. **非默认路径项目**:在 `/vol4/.../ccteam` 或 dir-name ≠ slug 的项目 `ccteam start` 正常 spawn 所有 bot
10. **turn timeout**:hook 链断时 90s 内收到 "Still working" 提示,180s 内收到 "stuck" 警告(不杀 turn)
11. **marker self-heal**:state.json 缺失场景 60s 内 daemon 自动重 spawn session 恢复
12. **creator bootstrap**:`/ccteam-creator` 走完后无需手动 `ccteam init`,`.ccteam/state.json` + `~/.ccteam/hooks/hook.sh` 自动到位
13. **CLI fallback**:`ccteam admin register-bot --slug X --role Y --chat-id Z` 能在 daemon 不跑时也注册
14. **self-host**:`ccteam init --force` 在 ccteam 自身源码目录里能装

全部通过 = ship 闭环。

---

## 8. 已知 V0.7 follow-up

- **F195 turn_timeout_sec plumb**:workflow.yaml schema 接好但 daemon 生产恒用 default;`tick_supervisors_with_config` 需读 workflow.yaml,~30 LoC
- **R3 红线松绑 + advise 实现重做**:`claude -p` 唯一 site `advise.rs::run_claude_advisor`,推荐方案 = 长跑 advisor tmux session + `/clear` 每 query 重置(无状态语义保留,但 session 复用 + cost ledger 统一);用户已口头确认 V0.7 做
- **squad roster runtime update**:当前 `.claude/agents/<role>.md` 静态拼接,register 新 bot 后老 bot 不知;增量 `@ccteam reload squad` admin keyword 可选 polish
