# V0.6.5 — PRD

17 finding · 3 Epic · doc-first 完整需求。

> **本 PR 范围**:本文件 + `README.md` + `dev-plan.md`(Wave 0 doc-first);代码改动全部走后续 Wave 1-3 PR。
>
> **Pre-v1.0 不留技术债**:实现里**不写 backward-compat shim**;`workflow.yaml` / BotRegistration schema 直接前进(no `#[serde(default)]` legacy compat unless 必要);CLI surface 直接扩(no deprecated alias);老用户的 manually-poked `~/.ccteam/imd/registry/` JSON 文件 schema 不变,因此自然兼容。

---

# EPIC E — MCP 桥 + creator 通路(P0)

**主线**:把 V0.6.0 立项时承诺的 chat-mode end-to-end onboarding 真正接通。今晚 2026-05-23 Telegram duplicate flood 调研的 root cause 不只是 OutboundCursor race(那是另一个 bug,V0.6.4 已修),更深层是 `/ccteam-creator` Phase 5.6 注册 bot 这条桥**自 V0.6.0 就没建过**。所有看到 ccteam chat bot 工作的项目都是老用户某时手动写过 `~/.ccteam/imd/registry/<slug>/<role>.json`。

## F146 — `mcp__ccteam__chat_register_bot` + `chat_list_bots` 真实现

### 痛点

`/ccteam-creator` Phase 5.6 documentation says "call `ccteam_imd::register_bot(slug, persona_id, vendor, im_platform, im_chat_id)`" ── Rust 函数存在,但 Claude TUI 没法直接调 Rust 函数,**MCP `chat_*` 工具全是 Wave 1 STUB**。skill 跑下来只写 `<project>/.ccteam/workflow.yaml`,**不写 BotRegistration JSON**,daemon `list_bots()` 返回空,所有 inbound 消息被 router dropped。

### 现状缺口

`crates/ccteam-cli/src/mcp_chat_tools.rs::dispatch()`:
```rust
match name {
    "ccteam__chat_send_input"
    | "ccteam__chat_lifecycle"
    | "ccteam__chat_reset"
    | "ccteam__chat_list_bots"
    | "ccteam__chat_history" => { ... "error": "NotImplemented" ... }
    _ => Ok(None),
}
```

5 个 chat tool 全在 stub 分支。原 `chat_lifecycle` 设计是单工具多 action(`start` / `stop` / `reset`)── 本版**拆为原子操作**,新增 `chat_register_bot` / `chat_unregister_bot`(`chat_lifecycle` 在 F151 删除,以 atomic verbs 取代)。

### 需求

**新增 MCP tool `ccteam__chat_register_bot`**:
- input schema:`{ workflow_slug: string, role: string, vendor: "claude"|"codex", im_platform: "telegram"|"slack"|"discord", im_chat_id: string, persona_id?: string }`
- 内部调 `ccteam_imd::register_bot()`,落 `~/.ccteam/imd/registry/<slug>/<role>.json`(0644;**non-secret** ── token 在 `~/.ccteam/im/credentials.json` 0600)
- 返回:`{ ok: true, path: "<absolute path>", workflow_slug, role }`
- 错误:invalid vendor / role 已存在(返 `ok: false, error: "already_registered"`,daemon 不冲突写)/ slug 含非法字符 → 400 等效

**新增 MCP tool `ccteam__chat_unregister_bot`**(F151 依赖):
- input schema:`{ workflow_slug: string, role: string }`
- 调 `ccteam_imd::unregister_bot()`
- 返回:`{ ok: true, removed: bool }`(`removed=false` 表示 idempotent miss)

**真实现 MCP tool `ccteam__chat_list_bots`**:
- input schema:`{ workflow_slug?: string }`(可选过滤)
- 调 `ccteam_imd::list_bots()`,可选 filter
- 返回:`{ ok: true, bots: [{ workflow_slug, role, vendor, im_platform, im_chat_id, created_at, running: bool, last_turn_at?: string }] }`
- `running` 字段由 `BotSupervisor::is_started()` 推断;daemon 未跑则 `running: false`、`last_turn_at` 从 `<project>/.ccteam/chat/<role>/turns.jsonl` mtime 读

### 文件(预估)

- `crates/ccteam-cli/src/mcp_chat_tools.rs` ── 拆 stub dispatch,新增 3 个真 dispatch 函数;chat_register_bot / chat_unregister_bot / chat_list_bots 三处
- `crates/ccteam-cli/src/mcp_serve.rs` ── tool 注册表新增 register / unregister(`chat_lifecycle` 移除,留 schema-version note 引导用户改用新工具)
- `crates/ccteam-imd/src/lib.rs` ── 已有 `register_bot()` / `unregister_bot()` / `list_bots()`,**不动**;新增 `bot_running_status()` helper 给 list_bots MCP 用(查 BotSupervisor registry)
- `crates/ccteam-imd/tests/chat_register_mcp_test.rs`(新)── end-to-end:tempdir `HOME` → MCP dispatch chat_register_bot → 读 disk → assert
- `docs/interfaces.md` ── §MCP 表 `chat_lifecycle` 行替换为 `chat_register_bot` / `chat_unregister_bot`

### 验收

1. `echo '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"ccteam__chat_register_bot","arguments":{"workflow_slug":"test","role":"helper","vendor":"claude","im_platform":"telegram","im_chat_id":"42"}}}' | ccteam internal mcp-stdio` → 落 `~/.ccteam/imd/registry/test/helper.json` 内容 schema-valid
2. 同 slug+role 再调一次 → 返回 `already_registered`,文件不被 clobber(idempotent miss = false)
3. `chat_list_bots` 不传 filter → 返回 1 行;传 `workflow_slug="other"` → 返回 0 行
4. `chat_unregister_bot` 调一次 → 文件消失;再调 → `removed: false`(idempotent)
5. baseline ≥ 1488 / 1(本 finding +6 测试)

### 风险

- **vendor 枚举 serde 大小写**:今晚踩过 ── BotRegistration `vendor: "Claude"` 大写被 daemon 拒收,只接受 `"claude"`。MCP tool 必须在 input schema 加 enum constraint(`"enum": ["claude", "codex"]`)且 dispatch 函数主动 lowercase。
- **`bot_running_status()` 跨 crate 依赖**:`ccteam_imd::SupervisorRegistry` 是 in-process `Arc<Mutex<...>>`;MCP 工具是新进程(stdin/stdout 模式),**读不到 daemon 的内存 registry**。Workaround:走文件 heartbeat ── `~/.ccteam/imd/registry/<slug>/<role>.heartbeat`(daemon 每 5s touch;mtime 在 30s 内 → `running: true`)。需在 daemon BotSupervisor 加 heartbeat-write 路径。
- **`chat_lifecycle` 移除是 breaking**:本 V0.6.5 sweep `/ccteam-creator` Phase 5.6 等所有 caller 同步改用 atomic verbs;old `chat_lifecycle` STUB 直接删,**不写 deprecated alias**(违反 CLAUDE.md §五 #4)。

---

## F147 — `mcp__ccteam__chat_send_input` + `chat_history` + `chat_reset` 真实现

### 痛点

剩下 3 个 `chat_*` STUB 同样阻塞外部 caller 给 chat-mode bot 发消息 / 读历史 / reset 卡死会话。F146 关掉了 register / list / unregister,本 finding 关 send / history / reset 三个。

### 现状缺口

同 F146 ── `mcp_chat_tools.rs::dispatch` 三个 match arm 还 return NotImplemented。

### 需求

**`ccteam__chat_send_input`**:
- input:`{ workflow_slug: string, role: string, content: string, reply_to?: string }`
- 实现:写 `<project>/.ccteam/chat/<role>/inbox/msg-<unix-ms>-<rand>.md` 文件(envelope: yaml frontmatter + content body),格式与 `daemon.rs` inbox dispatcher 期望一致
- 写完文件后**不**触发 tmux send-keys(daemon 自己 mpsc fast-path 会读到 inotify 事件)
- 返回:`{ ok: true, mailbox_path: "...", cid: "<cid>" }`

**`ccteam__chat_history`**:
- input:`{ workflow_slug: string, role: string, n?: number(default 20) }`
- 实现:读 `<project>/.ccteam/chat/<role>/turns.jsonl`,tail 最后 N 行(只取 `role: "assistant"` 行,user 行可选 ── 由 input flag `include_user?: bool` 控制,default false)
- 返回:`{ ok: true, turns: [{ turn_id, ts, role, content }] }`

**`ccteam__chat_reset`**:
- input:`{ workflow_slug: string, role: string }`
- 实现:写 `<project>/.ccteam/chat/<role>/signals/reset.signal` 文件;daemon BotSupervisor 监听该信号,执行 `close_thread` + `start_thread`(新 sid)
- 返回:`{ ok: true, signal_path: "..." }`(异步操作,不等 daemon 真 reset 完;用户 follow up 用 chat_list_bots 查 last_turn_at)

### 文件(预估)

- `crates/ccteam-cli/src/mcp_chat_tools.rs` ── 3 个新 dispatch 函数 + schema 描述真实化
- `crates/ccteam-imd/src/supervisor.rs` ── BotSupervisor 监 reset.signal 文件(新轮询 + inotify;可借用现成 `ArtifactWatcher`)
- `crates/ccteam-imd/tests/chat_send_input_test.rs`(新)── mailbox 文件落 + envelope 解析往返
- `crates/ccteam-imd/tests/chat_reset_signal_test.rs`(新)── reset signal → daemon close+start 真发生(用 stub adapter 验)
- `docs/interfaces.md` ── §MCP 表 send_input / history / reset 三行从 STUB → 真描述

### 验收

1. `chat_send_input` 调用 → mailbox 文件落 + daemon 5s 内 mpsc fast-path consume + tmux session 收到(stub adapter test)
2. `chat_history` 返回 last 20 assistant turns,与 turns.jsonl 内容一致
3. `chat_reset` 调用 → 1s 内 daemon 看到 signal → 旧 sid `close_thread` + 新 sid `start_thread` 都触发 + 旧 turns.jsonl 归档到 `<role>/archive/turns-<old-sid>.jsonl`
4. baseline ≥ 1496 / 1(本 finding +8 测试)

### 风险

- **reset 期间 outbound flood 风险**:reset 触发 → 旧 tmux 关 → 新 tmux 起 → claude 重新建 session jsonl。这正是 V0.6.4 修过的 Bug B(subagent jsonl re-emit)的近亲 ── 新 sid 不在 prior_offsets 里,从 0 开始读。Mitigation:reset 时主动 `cursor.force_set(0)` + `cursor.prior_offsets.clear()`,保证 fresh start 不替老 sid 重放(F147 实现里专门写一段)。
- **mailbox 文件名 race**:多个 MCP caller 并发调 send_input → 文件名 `msg-<unix-ms>-<rand>` 的 `<rand>` 必须用够长(8 hex char)避碰撞;`<unix-ms>` 用 `now_unix_ms()`(纳秒分辨率)。

---

## F148 — `/ccteam-creator` Phase 5.6 端到端跑通

### 痛点

`/ccteam-creator` skill 跑下来用户**以为成功了**(write 了 workflow.yaml + .claude/agents/<role>.md + 自定义 .mcp.json),但 BotRegistration 没落 → daemon `bots=0` → TG 消息全 dropped。今晚 nas-box005 调研事故的最深层 root cause。

### 现状缺口

`skills/ccteam-creator/SKILL.md:255`:

> *"For chat presets, call `ccteam_imd::register_bot(slug, persona_id, vendor, im_platform, im_chat_id)` → `Result<PathBuf>`."*

skill 是 LLM-driven 自然语言,**没有任何方式可以直接调 Rust 函数**。F146 实装 MCP 工具后这条桥才有真实现路径。

### 需求

`skills/ccteam-creator/SKILL.md` Phase 5.6 改写:
- 文案从 "call `ccteam_imd::register_bot()`" 改为 "invoke `mcp__ccteam__chat_register_bot(workflow_slug, role, vendor, im_platform, im_chat_id)`"
- 调用入参:`im_chat_id` 取自 Phase 5.1 写入 `~/.ccteam/im/credentials.json` 的 `allowed_chat_ids[0]`(creator skill 已有读凭证逻辑)
- 错误处理:`already_registered` → skill 决策为 idempotent OK 继续,不报错 retry;其他 error → stop + 告诉用户 "registry write failed:<reason>"
- **Phase 5.9 用户回执 + Phase 5.8 daemon ensure-running** 不变,但前者新加一行:"  Bot 注册到 ~/.ccteam/imd/registry/<slug>/<role>.json ✓"

`docs/quickstart.md` "Set up Telegram bot in 5 minutes" 段同步:命令顺序 = `/ccteam-creator` → go → 等 daemon 自动起 tmux → TG 立即可用。**不再**需要任何手动 JSON 编辑。

### 文件(预估)

- `skills/ccteam-creator/SKILL.md` ── Phase 5.6 段 + Phase 5.9 reply 模板
- `docs/quickstart.md` ── 5 分钟 TG bot 段(如已有则改写)
- `tests/e2e/creator_full_path_test.rs`(新)── shell 脚本驱动:fresh tempdir HOME → ccteam-im-setup mock(预填 creds.json)→ `/ccteam-creator` 模拟 dialogue(jq 拼 MCP call)→ assert workflow.yaml + agent.md + registration JSON 全落 + 全 schema-valid

### 验收

1. `tests/e2e/creator_full_path_test.rs` 通过(stub Telegram + stub claude tui)
2. **真机 host-probe**:nas-box005 fresh state(`rm -rf ~/.ccteam/ /home/rob/projects/test-creator/.ccteam/ ~/.claude/projects/-home-rob-projects-test-creator/`)→ 在 claude TUI 内跑 `/ccteam-creator "做个 TG 助理 bot"` → `go` → 不手工 touch 任何文件 → 从 TG 发 `hi` → 收到回复
3. baseline ≥ 1498 / 1(本 finding +2 测试)

### 风险

- **skill 行为 LLM 依赖**:skill 文本指示 Claude 怎么决策,但实际执行还是 Claude 自由发挥。本 finding 改 skill 文本是必要不充分条件;**真验证只能靠 host-probe**(F148 验收 #2)。
- **`mcp__ccteam__chat_register_bot` 注册时 daemon 没起**:可能场景 ── 用户先跑 `/ccteam-creator` 再起 daemon。F146 MCP 工具只写文件,daemon 后起也能从 disk 读 registry(`list_bots()` 路径)── 不冲突。

---

## F149 — `/ccteam` 总入口移除 Wave 2/3 fallback 提示

### 痛点

`skills/ccteam/SKILL.md` 还在显示 ── 

- line 28:`Wave 1 实用范围 ... 三 sub-skill body 占位`
- line 64:`Wave 2 F114 复活 ── Wave 1 fallback "请直接说 'wave2-not-ready'"`
- line 67:`Wave 3 F112 新建 ── Wave 1 fallback 同上`
- line 71-77:整段 "当路由目标是 Wave 2/3 才落地时,告诉用户 ..."
- line 95:`d → 走 advise 路径(Wave 1:Wave 3 缺失说明)`
- line 112:`Wave 2 复活后填实(目前 V0.5 砍掉态)`
- line 118-119:`⏳ sub-skill body 由后续 Wave 填实 / ⏳ host probe 5 sub-skill 验收`

V0.6.0 Wave 2/3 实际把 creator / im-setup / advise 三 sub-skill body 都写了 ── 这 6+ 处 stale 文案是 V0.6.0 doc-syncer 漏 sweep 的 drift,F146-F148 / F152-F154 ship 后这些更彻底过时,**用户看了会怀疑系统状态**。

### 现状缺口

文案对齐脏。skill body 早已不是 placeholder。

### 需求

`skills/ccteam/SKILL.md` 系统性 sweep:

| 原 (line) | 改成 |
|---|---|
| 28 "Wave 1 实用范围" 段 | 删除整段;改 short note: "7 intents 全部对应实工 skill;不再有 placeholder fallback" |
| 64 intent 2 "Wave 2 F114 复活" | 删 "Wave 2 F114 复活" 描述,留 `/ccteam-creator <NL>` 路由 |
| 65 intent 3 "Wave 2 F117 新建" | 同上 |
| 67 intent 5 "Wave 3 F112 新建" | 同上 |
| 71-77 "Wave 1 缺失 sub-skill 处理" 整段 | 删除 |
| 75 提示用户调 `mcp__ccteam__chat_send_input(...)` 的描述 | 留路径,删 "stub 返 NotImplemented" 注 |
| 95 "Wave 1:Wave 3 缺失说明" | 删括号 |
| 112 "Wave 2 复活后填实" | 删 |
| 118-119 双 ⏳ 行 | 改 ✅ 或删除(本 PR ship 后清,F162 完成后再清 119)|

Doc-syncer 角度:本 PR 落 + F146-F156 ship 后,line 119 同步删/打勾;F162 ship 后 line 118 标记 ✅。

### 文件

- `skills/ccteam/SKILL.md`

### 验收

1. `grep -E "Wave [123]|wave2-not-ready|sub-skill body 由|占位" skills/ccteam/SKILL.md` 全 0 命中
2. baseline 不退(纯文档)

### 风险

无。纯文本 sweep。

---

## F150 — `/ccteam-control` 接 `admin_*` MCP 验证

### 痛点

V0.6.1 F128 ship 了 `admin_change_persona` + `admin_add_tool` 两个 MCP 工具(`crates/ccteam-cli/src/mcp_admin_tools.rs` 是真 impl),但 `/ccteam-control` skill 是否实际**调用**到了它们 ── 不确定。`/ccteam-control` 是用户管理已有项目的入口,如果 skill 还在用 V0.5 旧路径(直接 shell 调 `ccteam ctl ...` 已删除的子命令),用户会撞墙。

### 现状缺口

需要审 `/ccteam-control` 当前 skill body 是否引用 `mcp__ccteam__admin_*` 名字、还是引用过时 CLI 子命令。

### 需求

1. **Audit** `skills/ccteam-control/SKILL.md`:列出所有用到的 ccteam 操作(暂停、恢复、看费、删项目、改 persona、加 tool)→ 每条标注当前实际触发哪条路径(MCP / CLI / shell)
2. **Bridge**:对每条 ccteam-control 操作,确保 skill 文本明确说调 MCP `admin_*` / `workflow_*` 工具(如已有);如还在用过时 CLI,改为 MCP-first(CLI 留作 fallback,标注 "if MCP not registered")
3. **Smoke test**:每条操作各 1 个 integration test(用 stub MCP)
4. 文档补:`docs/user-manual.md` "如何暂停 / 恢复 / 改 persona / 加工具" 段引用具体 MCP 工具名

### 文件

- `skills/ccteam-control/SKILL.md` ── 审 + 改
- `crates/ccteam-cli/tests/mcp_admin_smoke_test.rs`(新)── 6 个操作各 1 smoke
- `docs/user-manual.md` ── admin 段

### 验收

1. `grep -E "ccteam ctl|ccteam-ctl " skills/ccteam-control/SKILL.md` 0 命中(确认旧 CLI 路径全清)
2. 每个 admin_* MCP 工具至少有 1 个 skill-driven smoke case 跑通
3. baseline ≥ 1504 / 1(本 finding +6 测试)

### 风险

- 旧 CLI 子命令(`ccteam ctl pause` 等)在 V0.6.0 已删,但 skill 文本可能还引;改完后老 muscle memory 用户可能问"为什么没了"── `docs/user-manual.md` 单独加 migration note(指向 MCP 工具)。

---

## F151 — `ccteam remove --purge` 同步清 `imd/registry/`

### 痛点

`ccteam remove <slug> --purge` 当前清:
- `~/.ccteam/config.yaml::projects[]` 删 slug 行
- `~/.ccteam/progress/<slug>.jsonl`
- `~/.ccteam/inbox/<slug>/`
- `~/.ccteam/control/<slug>/`
- `<project>/.ccteam/`、`<project>/.claude/agents/`、`<project>/workflow.yaml`

**不清** `~/.ccteam/imd/registry/<slug>/`,导致 stale BotRegistration JSON 在 daemon `list_bots()` 还能读到,但 workflow.yaml 已删 → 路由命中后 daemon 找不到 BotSupervisor → 撞错。

### 现状缺口

`crates/ccteam-cli/src/commands.rs::cmd_remove()` 的 purge 路径列表里没有 `imd/registry/<slug>/`。

### 需求

1. `cmd_remove(..., purge=true)` 路径加一行:`rm -rf ~/.ccteam/imd/registry/<slug>/`
2. F146 落地后,**优先**走 `chat_unregister_bot` MCP 调用(每个 role)── 触发 daemon 看到 registry 文件消失 → BotSupervisor 自动 graceful shutdown + tmux 关
3. 如果 daemon 没起,fallback 直接 `rm -rf` ── 老 registry 自然失效
4. `--dry-run` mode 输出多一行 `imd/registry/<slug>/ (N JSON files)` 展示给用户

### 文件

- `crates/ccteam-cli/src/commands.rs` ── `cmd_remove` 函数 + dry-run output
- `crates/ccteam-cli/tests/ccteam_remove_test.rs` ── 新加 case:remove --purge 后 imd/registry/<slug>/ 不存在

### 验收

1. `ccteam remove dev-foo --purge --dry-run` 输出含 `imd/registry/dev-foo/ (1 JSON file)`
2. `ccteam remove dev-foo --purge` 后 `ls ~/.ccteam/imd/registry/dev-foo/` 返回 NoSuchFileOrDir
3. daemon 跑着时 remove --purge → 5s 内 daemon `bots=...` 数下降 + 对应 tmux session 消失
4. baseline ≥ 1506 / 1(本 finding +2 测试)

### 风险

- **多 role 同 slug**:`~/.ccteam/imd/registry/<slug>/` 可能有多个 role JSON;rm -rf 一次清完。MCP unregister 路径需对每个 role 单调。

---

# EPIC F — Advise + Codex critic(P1)

**主线**:V0.6.0 立项 F112 §A 承诺 Claude + Codex 并行 advisor,Wave 3 实际 ship 了 `CodexExecAdapter`(Rust 层 OK),但 MCP `advise_*` 工具表面留 STUB,`/ccteam-advise` skill body 调到 NotImplemented。

## F152 — `mcp__ccteam__advise_vote` 真实现

### 痛点

`/ccteam-advise "A vs B 该选哪个"` 跑完,后台调 `mcp__ccteam__advise_vote` → NotImplemented → skill 静默退化为纯 Claude 单边回答,违反 "Codex unavailable" 必须显式提示的红线(skills/ccteam-advise/SKILL.md "Red lines" §3)。

### 现状缺口

`crates/ccteam-cli/src/mcp_advise_tools.rs::dispatch()`:
```rust
match name {
    "ccteam__advise_vote" | "ccteam__advise_parallel" => {
        ... "error": "NotImplemented" ...
    }
    _ => Ok(None),
}
```

### 需求

**`ccteam__advise_vote`**:
- input:`{ question: string, context?: string, codex_timeout_secs?: number(default 60) }`
- 实现:并行 spawn 两个 advisor session
  - Claude 路径:走 in-process Claude API call(同 Task subagent 路径,sonnet,system prompt = `personas/critic.md`)
  - Codex 路径:`crates/ccteam-core/src/execution/codex_exec.rs::CodexExecAdapter::start_thread + submit_turn` 一次性 q&a(同 mode 2 bg 但只问一题)
- 等两边返回 → 第三次 Claude call 做 verdict synthesis(prompt:看双方 verdict 找 agreement / disagreement / suggest 1 winning approach)
- 返回:`{ ok: true, verdict: string, claude_answer: string, codex_answer: string|null, codex_status: "ok"|"timeout"|"unavailable"|"error", agreement: "agree"|"disagree"|"partial" }`
- **Codex 不可用** → `codex_answer: null` + `codex_status: "unavailable"`(reason: codex not in PATH / not authenticated / etc.),verdict 段必须显式说 "Codex unavailable: <reason>"(红线守)

**预算**:本调用计入 `~/.ccteam/cost-budget.json::advise_today_usd`;`max_cost_usd_per_24h`(per-vendor,沿用 V0.6.0 budgets)超顶 → 拒服务返 `ok: false, error: "budget_exceeded"`。

### 文件

- `crates/ccteam-cli/src/mcp_advise_tools.rs` ── dispatch 真实现
- `crates/ccteam-core/src/advise.rs`(新)── 提供 `advise_vote()` 函数,封装并行 spawn + synthesis 逻辑
- `crates/ccteam-cli/tests/mcp_advise_vote_test.rs`(新)── stub Claude API + stub `codex exec`,跑完整 vote 路径,验 verdict 输出 + agreement 标记 + Codex unavailable 路径
- `docs/interfaces.md` ── §MCP 表 advise_vote 从 STUB → 真描述

### 验收

1. 真路径(Codex installed + auth)→ verdict 包含 claude_answer 段 + codex_answer 段 + 1 段 synthesis
2. Codex unavailable(`CCTEAM_CODEX_BIN=/bin/false`)→ verdict 显式说 "Codex unavailable: <reason>",仍返 ok: true
3. budget 触顶 → returns budget_exceeded,**不**消耗 advisor 调用
4. baseline ≥ 1514 / 1(本 finding +8 测试)

### 风险

- **Codex CLI 协议漂移**:F144 已修 vendor forward-compat 解析,本 finding 沿用 ── codex stderr / exit code / `--json` 输出新字段时不 panic
- **并行 Claude API rate-limit**:Claude API quota 撞顶 → claude_answer:"<rate-limited>",verdict 文本必须涵盖该 case

---

## F153 — `mcp__ccteam__advise_parallel` 真实现

### 痛点

advise_vote 是 2 advisor + 1 synthesizer;advise_parallel 是 N-of-N 不合成。`/ccteam-team 3:reviewer` 这类 N-way 调用都路由到 advise_parallel,目前 STUB。

### 现状缺口

同 F152,dispatch arm 还是 NotImplemented。

### 需求

**`ccteam__advise_parallel`**:
- input:`{ question: string, n: number(2-8), vendors?: ("claude"|"codex")[](default ["claude","codex"]), timeout_secs?: number(default 60) }`
- 实现:fan out N parallel advisors(vendors round-robin OR explicit per-slot),不合成
- 返回:`{ ok: true, answers: [{ vendor, answer, status }] }`(N 项数组)
- 当 `vendors.len() < n` 时 round-robin 凑齐;`vendors.len() > n` 直接报错 invalid input

### 文件

- `crates/ccteam-cli/src/mcp_advise_tools.rs` ── dispatch
- `crates/ccteam-core/src/advise.rs` ── `advise_parallel()` 函数,基础设施同 F152(共享 spawn helper)
- `crates/ccteam-cli/tests/mcp_advise_parallel_test.rs`(新)── N=4 case 验返回 4 项 + N=8 vendors=["claude"] 全 claude 验证 + budget 触顶 case

### 验收

1. N=4 vendors=["claude","codex"] → 2 claude + 2 codex 答(round-robin)
2. N=2 vendors=["claude"] → 2 claude
3. budget 触顶 → 不消耗 advisor,err
4. baseline ≥ 1518 / 1(本 finding +4 测试)

### 风险

- N 大时并行起码消耗大,Claude API rate 限制可能撞,timeout 默认 60s 后未返单条 → status `timeout`,answer 段空字符串

---

## F154 — `/ccteam-advise` skill body 文案修正

### 痛点

`skills/ccteam-advise/SKILL.md:168`:

> "for N-way voting use the MCP tool `ccteam__advise_parallel` instead (**Wave 3 daemon**)."

line 186 同样指向 "the daemon registers (`ccteam__advise_vote` / `ccteam__advise_parallel`)" 描述,**没说这两工具其实 STUB**。F152 + F153 ship 后这两行同步改成真路径描述。

### 现状缺口

skill 文本承诺的能力大于实际能力。

### 需求

`skills/ccteam-advise/SKILL.md` 改:
- line 168 删 "(Wave 3 daemon)" 注 ── F152/F153 ship 后无需该注
- §"Where to look" line 186 ── 描述准确(已是真路径,只删 "the daemon registers" 加注)
- §"How this skill differs from ..." 段如果引用 STUB 状态 → 同步更新

### 文件

- `skills/ccteam-advise/SKILL.md`

### 验收

1. `grep -E "Wave [123]|STUB|NotImplemented" skills/ccteam-advise/SKILL.md` 0 命中
2. baseline 不退

---

## F155 — `ccteam-creator` Phase 3.5 Codex auto-critic 验证

### 痛点

`skills/ccteam-creator/SKILL.md` Phase 3.5 (V0.6.0 Wave 3 F112 §B) 描述 ── 当 persona 是 critic-flavor 时,skill 自动跑 `codex --version && codex login status`,通了就 inject `executor: codex` 到 workflow.yaml。**但这是 skill 文本 ── Claude 决定要不要执行,没有 deterministic gate**。是否真的工作过 → 未知。

### 现状缺口

无测试覆盖。可能完全 broken,可能巧合工作,文档说 "real-verified on remote" 但实际证据只在 V0.6.0 host-probe.md。

### 需求

1. **写测试**:`crates/ccteam-core/tests/creator_codex_critic_test.rs`(新)── 模拟 critic persona(如 `code-critic`)被选中,断言 workflow.yaml 渲染时 `agents.<role>.executor: codex` 出现(当 `$CCTEAM_CODEX_BIN` 设为 stub 返成功时);unset 时不出现
2. **加 deterministic gate**:`crates/ccteam-cli/src/commands.rs` 加 `ccteam doctor --check-codex-auto-critic` flag ── 跑同样 detection probe,exit 0 / 1 + stdout 输出 detected_state(用户和 skill 可同样 query)
3. **skill 改**:Phase 3.5 文本明确说 "consult `ccteam doctor --check-codex-auto-critic`,subprocess output → decide" 而不是说 "subprocess inline"

### 文件

- `crates/ccteam-cli/src/commands.rs` ── 新 doctor flag
- `crates/ccteam-core/src/templates/workflow_templates/` ── critic-flavor preset 模板加 `executor: codex` 占位 + tests
- `crates/ccteam-core/tests/creator_codex_critic_test.rs`(新)
- `skills/ccteam-creator/SKILL.md` ── Phase 3.5 段引用新 doctor flag

### 验收

1. `CCTEAM_CODEX_BIN=/path/to/working/codex ccteam doctor --check-codex-auto-critic` → exit 0 + stdout `{"available": true, ...}`
2. `CCTEAM_CODEX_BIN=/bin/false ccteam doctor --check-codex-auto-critic` → exit 1 + `{"available": false, "reason": "..."}`
3. test 覆盖:critic persona + codex_available=true → workflow.yaml 渲染含 `executor: codex`;codex_available=false → 不含
4. baseline ≥ 1522 / 1(本 finding +4 测试)

### 风险

- **codex CLI 版本检测**:不同 codex 版本 stdout 格式有差异;`--version` parse 用宽松 prefix match(如 startswith "codex ")

---

## F156 — `/ccteam-team` §3.5 N≥3 critic auto-injection 验证

### 痛点

`skills/ccteam-team/SKILL.md:143`:

> "When N ≥ 3 ... (`/ccteam-team 3:reviewer`) ... in V0.7) **will** route this through the daemon's `CodexExecAdapter` so the critic gets Codex evidence ..."

读起来像 "Wave 3 没做,V0.7 才做"。但 V0.6.0 Wave 3 ship 了 CodexExecAdapter 完整,V0.6.5 F152 又把 advise vote 接通 ── 该把这段含糊描述要么改"真路径已通"要么明确"V0.7 计划",不能两可。

### 现状缺口

V0.6.0 → V0.6.5 跨 5 个 patch 没人 review 这段。

### 需求

1. **Audit** `skills/ccteam-team/SKILL.md` §3.5 当前真实路径:N=3 时是不是真的会路由 critic 到 Codex?如否,**显式标注 V0.7+ deferral**(参考 ccteam-im-setup skill 拒 Slack/Discord 的格式)
2. **决策**:Codex critic auto-injection in `/ccteam-team` 是
   - (a) **本 V0.6.5 同步实现**(加 1 个 small test + 1 段 wave-2 spawn helper)── 若 advise_vote 已接通,这条几乎免费
   - (b) **明确 V0.7 推迟**(文案改清)
3. 用户 choose (a) → 加测试 + 实现;choose (b) → 仅文案 sweep

**决策默认 = (a)**:advise vote infra 在 F152 已 ready,team-3-reviewer 走同 helper 即可,工时小、价值大。

### 文件

- `skills/ccteam-team/SKILL.md` ── §3.5 文案改
- `crates/ccteam-core/tests/team_3reviewer_codex_critic_test.rs`(新)── stub adapter 验:N=3 reviewer team + Codex available → 至少 1 role 是 codex
- 可选:`crates/ccteam-core/src/templates/workflow_templates/team-3-reviewer.yaml` ── if Codex auto-critic injection 落进模板而非 skill 决策

### 验收

1. `grep "in V0\.7" skills/ccteam-team/SKILL.md` 0 命中
2. (如选 a)stub adapter test pass
3. baseline ≥ 1524 / 1(本 finding +2 测试,如选 a;选 b 则 +0)

### 风险

- 选(a)时需测试 `multi_workflow` 模板 schema 是否够灵活 ── 若需扩 schema,scope creep;选(b)保守

---

# EPIC G — UX cohesion + F113 收账

**主线**:用户上手 30 秒看到 value;决策树替代 mode/preset/recipe 三层抽象;dispatcher 文案对齐当前真实状态;F113 验收 #5 (50-query intent classification) 第一次真做。

## F157 — `ccteam-scan --quick` 60s 内出报告 + `/ccteam` 加 code-scan 入口

### 痛点

V0.6.2 ship 的 `ccteam-scan` 是大码库 audit(分层 CLAUDE.md / 噪声排除 / LSP / map 四项),跑全量 5-10 分钟。**新用户 30 秒内看到 value** 这条目标没有任何 ccteam 路径满足 ── quickstart 还在 lead with TG bot(需要 token / 5+ 分钟 onboard)。

### 现状缺口

`skills/ccteam-scan/SKILL.md` 设计目标是 audit-quality,不适合 60s zero-config 体验。

### 需求

1. `ccteam-scan` 加 `--quick` flag(default mode 仍是 audit)
2. `--quick` 启用 1 个 sonnet agent + 3 个固定问题:
   - 问 1:`ls -la` + `git ls-files | head -50` → 推断主语言 / 框架 / 入口文件
   - 问 2:`rg "TODO|FIXME|HACK"` → top 10 热点
   - 问 3:CLAUDE.md / README.md 存在?有则简介,无则建议初始化
3. 输出 `<repo>/.ccteam/codebase-scan.md`(同当前 path)── 但 frontmatter 加 `quick: true` flag,后续 audit mode 跑时升级
4. `skills/ccteam/SKILL.md` 增加第 5 个 intent(`code-scan`):"扫一下代码 / 摸底新项目 / scan code / audit codebase"
5. `/ccteam "扫一下代码"` → route to `/ccteam-scan --quick`

### 文件

- `skills/ccteam-scan/SKILL.md` ── 加 `--quick` mode description
- `skills/ccteam/SKILL.md` ── 加 intent 5 (code-scan) + 路由
- `tests/integration/scan_quick_test.sh`(新)── tempdir + git init + 几个 fake file → 跑 `/ccteam-scan --quick` → 验落地报告 + 内容覆盖 3 问题

### 验收

1. 在 `/tmp/test-rust-repo`(预装一个 sample crate)跑 `/ccteam-scan --quick` ── 90 秒内出 `.ccteam/codebase-scan.md`
2. 报告含三段(language/framework/entry, TODO hotspots, CLAUDE.md status)
3. `/ccteam "扫一下代码"` 路由到 scan ── intent classifier 测试(F162 fixture 加 5 条对应 sample query)
4. baseline 不变(skill 改不影响 cargo test)

### 风险

- **sonnet 1 agent 60-90s 不一定够**:fallback 是把 quick 改成 haiku 4.5(更快、便宜);trade-off 见 dev-plan §Epic G

---

## F158 — `docs/task-to-command.md` 决策树 + tier-1 docs lead 改写

### 痛点

`docs/orchestration-patterns.md` 讲 mode/preset/recipe 三层抽象,用户面对的真问题是 "我想做 X,用啥命令"。现状逼用户读架构文档才会用。

### 现状缺口

文档结构 contributor-centric,不是 user-centric。

### 需求

1. 新文档 `docs/task-to-command.md` ── 单屏决策树(草案见 README §Epic B):
   ```
   你想做的事                              → 用这个
   ──────────────────────────────────────────────────────
   摸底新代码库 / audit                     /ccteam-scan
   开发 / 修 bug / 重构(全程盯着)         /ccteam-team "<task>"
   review PR / 第二意见                     /ccteam-advise "<PR or path>"
   做个 IM 私聊助理(长期 24/7 在线)       /ccteam-creator "做个 X 助理"
   做个团队 IM 圆桌(多 bot 互动)          /ccteam-creator "群里几个 bot"
   夜里跑长任务(hands-off)                /ccteam-creator "<task>,关电脑跑"
   看 / 暂停 / 恢复 / 看花费                /ccteam-control list / pause / cost
   配 / 改 IM token                         /ccteam-im-setup
   不确定?用自然语言问                     /ccteam "<NL 描述>"
   ```
2. 嵌入 `docs/quickstart.md` 顶部第一节(覆盖原 TG bot 5 分钟段位置;TG bot 留下沉到 §2 "若你要做 IM bot")
3. 嵌入 `docs/user-manual.md` 顶部
4. **`README.md`(英文,root)** ── 加同样表格的英文版(根 README 必须英文 per CLAUDE.md §三)
5. `docs/orchestration-patterns.md` 加 frontmatter `audience: contributors`(向新用户标注)

### 文件

- `docs/task-to-command.md`(新)
- `docs/quickstart.md` ── 重写第一节
- `docs/user-manual.md` ── 加 lead 段
- `README.md` ── 加决策树(英文)
- `docs/orchestration-patterns.md` ── frontmatter

### 验收

1. `grep -E "mode:.*chat|preset:.*chat-pocket" docs/quickstart.md docs/user-manual.md README.md` 0 命中(架构词汇下沉到 advanced/)
2. 决策树 9 条全在新用户路径覆盖 7 个 skill
3. 用户视角:`cd /any/repo && claude && /ccteam` → dispatcher 第一句话 + 决策树第一行内容一致

### 风险

- 翻译 ── 根 README 英文必须 native,不能机器翻译。本 finding 决策树短,翻译 effort 可控。

---

## F159 — `/ccteam` 对未实现 intent 直接隐藏

### 痛点

`/ccteam` 当前若识别 intent 落在尚未实现的路径(F146-F156 之前的 advise / chat),用户跑下来撞 NotImplemented。**用户从 dispatcher 选了一个 → 死胡同**,体验最差的 1 种。

V0.6.5 ship 后 ── F146-F156 实装,advise / chat 都通。本 finding 是 forward-looking:任何后续 V0.6.6+ 引入新 intent 但还没真实现的,**dispatcher 必须直接不暴露**,不打 placeholder。

### 现状缺口

`skills/ccteam/SKILL.md` 历史是"暴露 + warning",现在该转"未实现就不显示"。

### 需求

1. `skills/ccteam/SKILL.md` 加 §"Hide 未实现 intent" red line ── 任何未 ship 的 intent 不出现在 dispatcher 4-options 里,也不出现在路由表
2. Each new sub-skill ship 时 ── 必须先确认 MCP 工具表面 + skill body 都 ready 才能加进 `/ccteam` intent 表
3. 文案 review:`skills/ccteam/SKILL.md` 第 1-30 行的 intent 介绍只列 ship 状态的

### 文件

- `skills/ccteam/SKILL.md`

### 验收

1. `/ccteam` 跑下来 4-options 只列已 ship 路径(V0.6.5 ship 后是 7 个 intent 全 ship,4-options 应根据 NL 推断动态选 4 个最相关)
2. `grep "STUB|NotImplemented|placeholder|fallback.*not.*ready" skills/ccteam/SKILL.md` 0 命中

### 风险

- 决策点:**dispatcher 看到 ambiguous intent 仍要给用户 4 options 选**,即使一个 intent 是未来 ── 这与本 finding 冲突。妥协:V0.6.5 内,4 options 只从 7 个 ship intent 里选;V0.6.6+ 新 intent ship 后再扩。

---

## F160 — CLAUDE.md §一 baseline 更新 + skill 状态注释清理

### 痛点

`CLAUDE.md §一 当前状态(2026-05-23)` 表里:
- `Workspace version` 行还是 `0.6.3`(V0.6.4 ship 后没回填)
- `当前最新版` 行还是 V0.6.3
- `测试 baseline` 行 `1471/1` 应是 1482/1(V0.6.4 后)→ V0.6.5 ship 后 ≥ 1530/1
- §四 Skills 段 6 sub-skill 列表 ── 含 ccteam-scan(✓)+ ccteam-im-setup(F117 一次性 IM token onboarding)── status 描述要核对(F117 描述里 Slack/Discord 推 V0.7 这件事是否说清)

V0.6.4 ship 之后这个 §一 表也没回填(我作为 maintainer 漏的,不只是本 V0.6.5)。

### 现状缺口

CLAUDE.md 是每 session 自动加载的 SoT,baseline 数字不对会让下一个接手 Claude 浪费 time 重新校对。

### 需求

V0.6.5 ship 后(Wave 4):

| Cell | 老 | 新 |
|---|---|---|
| HEAD | `83f8bce`(V0.6.3) | (V0.6.5 ship commit SHA)|
| Workspace version | `0.6.3` | `0.6.5` |
| 测试 baseline | `1471/1` | (V0.6.5 实际数,目标 ≥1530/1)|
| Clippy | `0 errors + 0 warnings` | 不变 |
| 代码规模 | `~73 kLOC Rust` | 实际新增后数,大致 ~74-76 kLOC |
| 当前最新版 | `V0.6.3` | `V0.6.5`(并加 "完整 chat onboarding ship" 一句话总结)|
| 上一版 | V0.6.2 | V0.6.4(回填 V0.6.4 OutboundCursor 一行)|
| V0.6.x 延期候选 | `空` | `空`(本版闭所有 retained risk)|
| V0.7 主线候选 | (不变,或更新)| 加 "Slack/Discord onboarding"(F146/F147 实装后 V0.7 解锁)|

§四 Skills 表 ── ccteam-control / ccteam-advise / ccteam-im-setup status 描述 sync 到 V0.6.5 真实状态(`/ccteam-control` 实际接 admin_* MCP;`/ccteam-advise` 实装 vote/parallel;`/ccteam-im-setup` Telegram only,Slack/Discord V0.7)。

### 文件

- `CLAUDE.md` ── §一 表 + §四 skills 表

### 验收

1. Workspace HEAD = V0.6.5 ship 后 SHA,§一 表所有数字正确
2. §四 skills 表 status 描述 = 实际能力,无"V0.7 才落地"占位

### 风险

无。doc-only。

---

## F161 — `/ccteam` dispatcher 文案 drift sweep

### 痛点

详见 §F149。**本 finding 是 F149 + F154 + F156 文案 sweep 的"用户面文档"配对** ── 这些 SKILL.md 改完后,docs/{quickstart,user-manual,recipes,troubleshooting,advanced/}/*.md 里如果还有引用"Wave 2/3 才落地"/"sub-skill body 占位"/"NotImplemented stub" 这类老话术,需要同步删 / 改。

### 现状缺口

User-facing docs 可能有 stale 提法。

### 需求

跨 docs/ root + skills/ + README.md grep:

```bash
grep -rn -E "Wave [123]|wave2-not-ready|sub-skill body|占位|STUB|NotImplemented" \
  docs/{quickstart,user-manual,recipes,troubleshooting}.md \
  docs/advanced/*.md \
  skills/*/SKILL.md \
  README.md
```

每条命中必须二选一:
- (a) 标注为 "V0.6.x ship 前" 状态 → 直接删除(这是 V0.6.5 ship 后的状态)
- (b) 标注是真实当前限制(如 V0.7 Slack onboarding)→ 改成 "V0.7+" 而非 "Wave X"

`docs/versions/v0-6-*/` 历史归档**不**动 ── 历史描述保留。

### 文件

- 所有 grep 命中的文件

### 验收

1. 上述 grep 在 tier-1 user-facing 文档 0 命中
2. baseline 不变

### 风险

无。

---

## F162 — F113 验收 #5 补做:50-query intent classifier ≥90% accuracy

### 痛点

`docs/versions/v0-6-0/prd.md:61` 是 F113 ship gate #5:

> "host probe:5 个 sub-skill 各 1 host test,intent classification accuracy ≥90%(基于 50 个 sample query)"

V0.6.0 Wave 4 ship 时仅跑了 5 preset E2E + 3 Codex scenario(`host-probe.md` 表),**完全没碰** 50-query intent test。V0.6.1-V0.6.3 patch 也没人补。这是 ccteam 旗舰功能(NL dispatcher)的核心验收数字 ── **从未存在**。

### 现状缺口

仓库全 grep:`sample_queries` / `intent_classifier` / `intent-test` 0 命中。

### 需求

1. **写 corpus** `tests/intent-corpus.yaml`:
   ```yaml
   # 50 NL queries → expected intent label (per /ccteam SKILL.md routing table)
   # 7 intent labels: start-team, create-workflow, configure-im, monitor,
   #                  advise, status-debug, code-scan(V0.6.5 新增)
   queries:
     - input: "我想做个 TG 助理 bot"
       expected: create-workflow
     - input: "fix all TS errors"
       expected: start-team
     - input: "暂停 dev-foo"
       expected: monitor
     - input: "为啥撞 budget"
       expected: status-debug
     - input: "扫一下这个代码库"
       expected: code-scan
     - input: "Claude + Codex 都看看 A vs B"
       expected: advise
     # ... 共 50 条,每 intent 至少 5-8 条,涵盖中英混合 + 不同句长 + 同义词 + 错别字
   ```
2. **写 runner** `scripts/host-probe/intent-accuracy.sh`:
   ```bash
   #!/bin/bash
   # 跑 corpus 过 /ccteam dispatcher intent classifier
   # 输出 confusion matrix + overall accuracy + per-intent precision/recall
   # 把结果落到 docs/versions/v0-6-5/intent-accuracy.md
   # exit 0 if accuracy ≥ 0.90 else exit 1
   ```
3. **运行方式**:driver 是 claude TUI session 处理 corpus 每条 query 后输出 routed intent;runner 把 50 个 输入 喂给 Claude,从输出抓 intent label(模式 = `routed_intent: <label>` 一行,或类似机器可读约定),对比 expected。**避免 50 次真实 claude 跑得太慢/太贵** ── runner 可选 mock(stub claude `claude --print --output-format json`)模式只跑 SKILL.md 路由表静态 match,不真起 sonnet。
4. **结果落地** `docs/versions/v0-6-5/intent-accuracy.md`:
   - 总 accuracy 数字
   - confusion matrix 7×7
   - 失败 case 列(`expected vs predicted` + Why,人工归因)
   - Ship gate:≥ 90% 通过,< 90% block ship

### 文件

- `tests/intent-corpus.yaml`(新)── 50 query
- `scripts/host-probe/intent-accuracy.sh`(新)── runner
- `docs/versions/v0-6-5/intent-accuracy.md`(新)── 结果归档
- `crates/ccteam-cli/src/commands.rs`(可选)── `ccteam doctor --intent-accuracy` flag 调 runner,formatted output

### 验收

1. runner 跑完 50 条 ── overall accuracy ≥ 0.90
2. confusion matrix 落 `intent-accuracy.md`
3. failed case 全部人工归因(模糊 input?intent overlap?SKILL.md 路由表不准?)
4. 任何 < 90% 的 intent → 单 finding 跟,**不**阻 V0.6.5 ship(但 ship gate doc 标注)
5. baseline 不变(runner 是 host probe,不进 cargo test)

### 风险

- **真跑 50 次 claude API call 成本**:估算 sonnet 4.6 input ~50 tok x 50 = 2500 tok input;output ~50 tok x 50 = 2500 tok output;按 $3/Mtok in + $15/Mtok out → < $0.10。**可接受**,但 host-probe 模式默认 mock(静态路由表 match)+ `--real` flag 走真 LLM 验证。
- **corpus 偏 ── 50 条可能不能代表 production**:V0.6.5 ship 后跟踪真实 user query,收集 / 扩 corpus 到 V0.6.6 / V0.7。

---

# 跨 finding 风险表(必读)

| 风险 | 影响 | 缓解 |
|---|---|---|
| F146 `bot_running_status` 跨进程 heartbeat 文件 race | daemon + MCP 同写一个 heartbeat 路径 | daemon **只写**,MCP **只读**;mtime 30s 内 = running |
| F146 vendor 大小写 `Claude` vs `claude` 已踩坑 | 注册失败静默 | MCP schema enum + dispatch 主动 lowercase |
| F147 reset 期间 outbound flood 风险 | 又像 Bug B 闪现 | reset 触发时 OutboundCursor `force_set(0)` + `prior_offsets.clear()` |
| F148 host-probe e2e 依赖 daemon 起 + tmux | 起不来 / hang | scripted health-wait 同 V0.6.1 F119 pattern;timeout 30s |
| F151 daemon 跑着 remove --purge 行为 | tmux session 怎么干净关 | MCP unregister 先调,daemon graceful 关 tmux + delete 文件;daemon 不在则 fs 删 |
| F152 Codex CLI 协议漂移 | advise vote 解析崩 | V0.6.3 F144 forward-compat 解析已 ship,沿用 |
| F155 doctor `--check-codex-auto-critic` 检测假阳/阴 | creator 误判 | stub binary tests + 真机 host-probe 双管 |
| F162 50-query corpus 偏 | accuracy 数字不代表 production | corpus 设计 review;default mock + opt-in real LLM run |

---

# Ship gate(同 README §5,prd 落实化)

V0.6.5 → main merge 前必须:

1. `cargo test --workspace --locked --no-fail-fast` ≥ **1530 / 1**
2. `cargo clippy --workspace --all-targets --locked -- -D warnings` 0 命中
3. F148 host-probe(fresh nas-box005):`/ccteam-creator "做个 TG 助理"` → `go` → 不手工 touch 任何文件 → 从 TG 发 `hi` → 收到回复,**手动签字**记入 `docs/versions/v0-6-5/host-probe.md`
4. F162 `intent-accuracy.md` 落,accuracy ≥ 0.90
5. tier-1 docs 文案 grep(F149/F154/F161 mandate)0 命中
6. `CLAUDE.md §一 baseline` 表已更新(F160)
7. `ccteam doctor` 报告 "MCP tool surface: 26 active, 0 stubs"
8. workspace version bump `0.6.4 → 0.6.5`;commit 前缀 `v0.6.5:`
9. git tag `v0.6.5`
