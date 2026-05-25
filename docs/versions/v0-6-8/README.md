# ccteam V0.6.8 — squad fan-out + chat handle ship-blocker patch

> **范围**: 两个 P0 ship-blocker 联袂 patch ── (1) chat-squad fan-out: 3 bot 重复回复同一条 IM 消息 → squad 模式完全不可用;(2) chat handle 行为与 doc 不符: SKILL 描述 nickname 池(`@curie` 等),代码用 `b.role`,用户拿 doc 给的 handle 发出去无响应。同 patch 附 `install.sh` self-install polish。**触动业务码**(与 V0.6.7 零业务码 patch 不同),仍保持 minor-flavored 范围。

---

## 1. 为什么紧急

### 1.1 Fan-out: chat-squad 不可用

`reno-squad` 工作流(3 chat-mode bot:customer-support / tech-helper / sales)群里 @ 任意一个 → 3 个 bot 都把同一条 assistant 回复 forward 到 Telegram。

**根因**(三层叠加):

1. `crates/ccteam-core/src/execution/claude_tui.rs::tail_loop` 收到的 `role` 参数只用来定位 cursor 文件,**没用来 target 哪条 jsonl**。内部 `discover_active_session(cwd)` 取 project dir 内最新 jsonl ── 3 bot 共用同一 project dir,谁先 turn 完成,3 个 tail loop 都读到同一份。
2. 生产路径**完全没注入** `CCTEAM_CHAT_ROLE` env(只有 `crates/ccteam-hooks/tests/chat_progress_test.rs` 在 `set_var`);`derive_role_from_payload` 始终走 `None` 分支 → 所有 chat-mode progress 事件 `role: ""`。
3. 实测 `~/.claude/projects/<encoded>/<UUID>.jsonl` ── Anthropic 的 `--name` 只是内部 session 索引 label,**不映射到文件名**。F172 V2 设计假设错了,所以"target deterministic `<chat_session_name>.jsonl`"这条捷径不通。

**唯一干净修法 = hook-plumb**: spawn 注入 env → hook 子进程继承 → SessionStart hook 拿真 session_id 写 marker → tail 读 marker target 那条 jsonl。

### 1.2 Handle = role: SKILL 与代码不符

- F114 给了 `agent_naming::SCIENTIST_NAMES` 池,`ccteam-creator/SKILL.md:322` 描述 scientist nickname 派发
- 但 `BotRegistration`(`ccteam-imd/lib.rs:54`) 不存 `chat_handle`,`daemon.rs:593 build_handle_map` 只用 `b.role`
- V0.6.6 F168 留 `TODO(V0.7-chat-handle)` 锚点准备 V0.7 落地;实际用户已经在用 → 必须提前到 V0.6.8

补充用户实战发现的衍生坑:**unknown @handle 完全静默 drop** ── 拿错 handle 发出去什么都没有,比报错更困惑。

---

## 2. Findings

### 2.1 Fan-out fix(F175 / F176 / F177)

| # | 改动 | 触动 | LoC |
|---|---|---|---|
| **F175** | tmux spawn 注入 `CCTEAM_CHAT_ROLE` + `CCTEAM_CHAT_SLUG` env;`TmuxSession::start` 加 env 重载(`tmux new-session -e KEY=VAL`) | `crates/ccteam-core/src/tmux.rs`,`crates/ccteam-core/src/execution/claude_tui.rs` | ~80 |
| **F176** | `chat_progress::handle_chat_progress` 在 `session-start` / `session-end` 写 `<project>/.ccteam/chat/<role>/active-session-id`(atomic rename) | `crates/ccteam-hooks/src/chat_progress.rs` | ~40 |
| **F177** | `tail_loop` 读 marker target 确定 jsonl;**删 `discover_active_session` 回退**;marker 缺失 → sleep+retry 等 SessionStart fire | `crates/ccteam-core/src/execution/claude_tui.rs`,`crates/ccteam-core/src/execution/transcript_tail.rs` | ~120 |

### 2.2 Handle fix(F180 / F181 / F182 / F183 / F184)

| # | 改动 | 触动 | LoC |
|---|---|---|---|
| **F180** | `AgentSpec.chat_handle: Option<String>` schema(workflow.yaml additive,缺省 = role) | `crates/ccteam-core/src/workflow.rs` | ~50 |
| **F181** | `BotRegistration.chat_handle: Option<String>` + `chat_register_bot` MCP 加入参 + atomic 持久化 | `crates/ccteam-imd/src/lib.rs`,`crates/ccteam-cli/src/mcp_tool_groups.rs` | ~80 |
| **F182** | `build_handle_map` 用 `chat_handle.unwrap_or(role)`;跨 slug collision → `<handle>@<slug>` 命名空间裁决 | `crates/ccteam-imd/src/daemon.rs`,`crates/ccteam-imd/src/router.rs` | ~50 |
| **F183** | `ccteam-creator` Phase 5.5/5.6 接 `agent_naming::pick_nickname()` auto-mint scientist handle → 写入 workflow.yaml + `chat_register_bot` 入参 | `skills/ccteam-creator/SKILL.md` + implementation crate(`crates/ccteam-cli/src/skill_dispatch.rs` 或同位) | ~100 |
| **F184** | router 未知 `@handle` → reply `Unknown handle '@xxx'. Available: @alice @bob @carol`;`@ccteam list bots` admin keyword(V0.6.1 F129 5→6 keyword) | `crates/ccteam-imd/src/router.rs`,`crates/ccteam-imd/src/nl_admin.rs` | ~120 |

### 2.3 Docs + install polish

| # | 改动 | 触动 | LoC |
|---|---|---|---|
| **F178** | 用户面 docs 凡描述尚未实现 behavior 段落 → 删;V0.6.8 实现后 docs 用**现在时**陈述(无 `V0.6.8 引入` 措辞,守 §三红线);删 SKILL 里 `ccteam internal daemon ensure-running`(命令不存在)失效引用 | `docs/{quickstart,user-manual,recipes,troubleshooting}.md`,`README.md`,`skills/` | ~30 |
| **F179** | `install.sh` 加 (a) 已最新短路(`ccteam --version` == TAG 跳过下载);(b) `pgrep -f "ccteam start"` daemon-running warning。**不加** uninstall path | `install.sh` | ~20 |

### 2.4 chore: V0.7 TODO 锚点 closeout

- 清 `crates/ccteam-imd/src/daemon.rs:582` 的 `TODO(V0.7-chat-handle)` 锚点
- `crates/ccteam-cli/tests/no_silent_todo_test.rs` 计数 6 → 5,assertion 更新
- `docs/dev-coupling-audit.md` chat_handle 条目从 V0.7-deferred → V0.6.8-closed

---

## 3. 红线核对(CLAUDE.md §三)

| 红线 | 触及? | 说明 |
|---|---|---|
| R0 — 文件系统是控制平面 | ✅ 强化 | F176 引入新 marker 文件 `<project>/.ccteam/chat/<role>/active-session-id`,**仍是文件系统 SoT** |
| R1 — progress.jsonl 唯一 state SoT | ✅ 守 | F175/176 让 progress 事件 `role` 不再 `""`(以前是 bug),SoT 完整性反而**修复** |
| R2 — No prompt injection | ✅ 守 | env 注入不是 prompt 注入 |
| R3 — 每次 spawn = fresh 1M context | ✅ 守(chat 模式不适用) | F177 仍走 `--name` / `--resume` 路径 |
| R4 — 永不主动 kill 长 session | ✅ 守 | 不动 |
| R5 — 不解析 tmux 终端输出 | ✅ 守 | tail 读 jsonl(SoT),hook 读 stdin JSON payload |
| R6 — fix-loop 撞 3 次 escalate | ✅ 守 | 不动 |
| R7 — ccteam-core 零 team 名字面量 | ✅ 守 | `chat_handle` 是 schema 字段,非 hardcode team 名 |
| R8 — 跨项目记忆走官方接口 | ✅ 守 | 不动 |
| R9 — 新建项目走 `<projects_root>/<team>-<slug>/` | ✅ 守 | 不动 |
| R10 — root README MUST be English | ✅ 守 | README 无版本号 / 状态信息 |
| R11 — README 不含版本进展/状态 | ✅ 守 | F178 doc 改动按现在时陈述,无 `V0.6.8 引入` 措辞 |
| R12 — HITL approval state SoT | ✅ 守 | 不动 |

---

## 4. Baseline gate

- workspace `0.6.7` → `0.6.8`(`Cargo.toml::workspace.package.version`)
- test: V0.6.7 baseline `1639/1`,本版新增至少 3 个 integration test(`claude_tui_fanout_test.rs` / `handle_resolution_test.rs` / `unknown_handle_reply_test.rs`),目标 ≥ `1645/1`(粗估,实际以 worktree 收尾为准)
- clippy: `-D warnings` clean
- fmt: `cargo fmt --all -- --check` clean

---

## 5. Ship gate(CLAUDE.md §五.7)

| Item | Done |
|---|---|
| `CLAUDE.md §一` baseline / workspace version 更新 | 待 |
| `Cargo.toml::workspace.package.version` 0.6.7 → 0.6.8 + `Cargo.lock` 同步 | 待 |
| `docs/versions/v0-6-8/README.md` 落地 | ✅(本文件) |
| `docs/dev-coupling-audit.md` 加 F175-F184 索引 + chat_handle TODO closeout | 待 |
| 用户面 docs(`README.md` / `docs/{quickstart,user-manual,recipes,troubleshooting}.md`)反映 chat_handle / scientist nickname 行为(现在时陈述) | 待 |
| 3 worktree PR 全绿 | 待 |
| 用户验:`reno-squad` 3 bot 群发不再 cross-fire;`@curie` 默认 handle 起效;`@<wrong>` 给出 available bots 列表 | tag-ship 后验 |

---

## 6. 不在范围(V0.7 候选,本版不做)

- Epic IM-Providers: Slack inbound + Socket Mode + WeChat / 飞书 / DingTalk / QQ(`TODO(V0.7-{im-providers,slack-inbound,slack-socket-mode})` 仍留)
- Epic Doctor/Init UX: `ccteam self-update` CLI + lazy update notifier + `ccteam admin register-bot` CLI fallback + daemon spawn 前 state.json 自补 + creator/daemon 项目落地路径一致化
- monorepo-aware `.mcp.json`
- migrate-from-claude
- 6 号编排模式深化(HumanApprovalAdapter full wrapper,`TODO(V0.7-human-approval-adapter)` 仍留)
- `/ccteam-creator` 完整 template library + LLM-assisted role auto-gen
- `listbots-cache` 优化(`TODO(V0.7-listbots-cache)` 仍留)
- **uninstall 子命令 / `install.sh --uninstall`**: 明确不做(pre-v1.0 重装频率低,一行 `rm -rf ~/.ccteam/` 文档说明更透明)

---

## 7. Acceptance(用户验)

V0.6.8 tag push → 3 PR 合并 → 重装:

```sh
ccteam stop 2>/dev/null || pkill -f "ccteam start" || true
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version  # 应输出: ccteam 0.6.8
ccteam start
```

squad 验证:

1. `reno-squad` 群 @ 一个 bot → **只有那一个** bot 在群里回复(其余 2 个 0 输出)
2. `@curie hello` / `@galileo hello` 等 scientist nickname 默认起效(若 ccteam-creator 给 reno-squad mint 的 nickname 是这些)
3. `@unknownname hello` → 群里收到 `Unknown handle '@unknownname'. Available bots in this chat: @<...> @<...>`
4. `@ccteam list bots` → 群里收到当前 chat 内所有可用 bot handle 列表

成功 = patch 闭环。
