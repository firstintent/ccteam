# V0.6.0 — 三执行模式定型 + 模式 3 (chat / IM) 落地 + MCP 优化

> **立项主线**:把"ccteam 是 Claude Code 之上的编排层"这句话**显式拆成三种执行模式**,每种锚到 Claude Code 一个执行原语;红线按模式重新 scope;模式 3(chat / IM bot)是本版新落地能力;MCP 顺带按 OMC 借鉴优化。
>
> **不立项**:任何会破坏现有模式 2(artifact-driven workflow)行为的改动。Wave 1 trait 抽离要求 `cargo test --workspace` 数字**完全持平**,不得退步。
>
> **doc-first 原则**:本 PR 只 land `docs/v0-6-0/`(README + PRD + dev-plan),代码改动按 3 个 wave 跟进 PR。

---

## 一、三执行模式定型(本版核心)

ccteam 在 Claude Code 之上提供三种 agent 执行模式,**锚到 Claude Code 三种执行原语**:

| 模式 | 直观场景 | spawn 原语 | input 面 | output 面 | context 生命周期 |
|---|---|---|---|---|---|
| **1. in-proc team** | CC 插件 / skill,一句话起临时 team(`/ccteam-team "fix TS errors"`)| `Task(subagent_type=…)` | tool args | tool result | parent session 内,死则同死 |
| **2. bg sessions** | 长跑自动化工作流(qa-loop:test→fix→release)| `claude --bg --agent <role>` | `.ccteam/inbox/*.md`(artifact)| `progress.jsonl` + state.json | **每次 spawn fresh 1M context** |
| **3. tmux sessions** | 随叫随到 chat agent team(TG 群里多 bot,@ 召唤,bot 间互 @)| 长跑 `claude`(交互模式,常驻 tmux pane)| `tmux send-keys`(含 `/new` `/compact`)| `~/.claude/projects/<…>/<session-id>.jsonl`(Anthropic 官方 session 格式)| `/new` 重置 / `/compact` 摘要,**在线管理** |

每模式自带的 control plane / cost model 详 `prd.md §F106`。

---

## 二、红线按模式重新 scope(配套改动)

现有 7 条红线大多写于 V0.1-V0.4 的模式 2 语境,V0.6 显式标注每条**适用哪些模式**:

| 红线 | 当前措辞 | 模式 1 | 模式 2 | 模式 3 |
|---|---|---|---|---|
| 文件系统是控制平面 | 全局 | N/A(in-proc)| **输入 + 输出都是** | **输出 SoT 是**(session jsonl);输入是 send-keys |
| `progress.jsonl` 唯一 state SoT | 全局 | N/A | **是**(SoT)| **业务事件 SoT**;对话原文走 session jsonl,**不重抄** |
| 每次 spawn = fresh 1M context | 全局 | parent session,无 spawn | **守** | **不适用** — chat 场景就是要复用,显式 `/compact` `/new` 管理 |
| 不解析 tmux 输出 | 全局 | N/A | **守** | **守** — 读 session jsonl 替代,**不 scrape pane** |
| 永不主动 kill 长 session | 全局 | N/A | **守**(F84 budget cap 唯一例外)| **守** + `/compact` `/new` 是合法状态操作,非 kill |
| fix-loop 3 次必 escalate | 全局 | parent 自决 | **守** | **守** + bot-to-bot @ ping-pong **hop_limit 同等 escalate** |
| `ccteam-core` 零 team 名字面量 | 全局 | **守** | **守** | **守** |

详 PRD §F106。

---

## 三、模式 3 是本版的新落地

模式 1 已由 V0.5.0 `ccteam-team` skill 落地;模式 2 是 V0.4.x → V0.5.x 主线。**模式 3 之前没有**。

落地范围:
- `ExecutionAdapter` trait 抽出(F107) — 现有 `BgSpawner` 改造对接,**行为零变化**。模式 3 / 模式 1 复用此 trait
- `TmuxInteractiveAdapter`(F108) — 长跑 `claude` 进程 + send-keys 输入 + session jsonl tail 输出
- `ccteam-im-bridge` crate(F109) — 第一个目标 IM 是 Telegram(`tgbot` crate 已成熟);Slack / Discord / IRC 留 trait 扩展点

**用户视角**:
```
$ ccteam new my-tg-bots --mode chat                # 新模式
$ vim .ccteam/workflow.yaml                        # 声明 bot roster + IM channel binding
$ ccteam start my-tg-bots
# 然后 TG 群里 @bot-name "..." → bot 在 tmux 里 long-running claude session 处理 → 回 TG
# bot-to-bot @ 自动路由
```

模式 3 **不复用模式 2 的 inbox watcher**(那是 fresh-context 触发,模式 3 是 stdin 喂)。但**复用** progress.jsonl(业务事件)/ MCP 控制面(user 远程看哪个 bot 在干嘛)/ web UI(panel 加 Chat View tab)。

---

## 四、MCP 优化(顺带)

借鉴 OMC `mcp__t__*` / `mcp__team__*` 模式,**单 server + 子前缀**重组:

| 改动 | 现状 | V0.6.0 后 |
|---|---|---|
| Server name | `ccteam` | `ct`(节省 4 字符 × 17 工具 × N session)|
| Tool 命名 | `mcp__ccteam__ls` `mcp__ccteam__spawn_agent` 平铺 | `mcp__ct__workflow_ls` `mcp__ct__workflow_spawn_agent` `mcp__ct__chat_send_input`(子前缀)|
| 选择性禁用 | 无,17 工具全发布 | `CCTEAM_DISABLE_TOOLS=chat_*,screenshot` env-driven |
| 项目级注册 | 只 `~/.claude.json`(user-global)| `ccteam init` 落项目 `.mcp.json`;user-global 保留作 fallback |

**Breaking**:`mcp__ccteam__*` → `mcp__ct__*`。pre-v1.0 不留 alias(CLAUDE.md §五:no backwards-compat shim)。meta-agent / `ccteam-control` skill / 测试同步改。

---

## 五、Findings 索引

| F | 主题 | 性质 | Wave |
|---|---|---|---|
| F106 | 三执行模式定型 + 红线按模式 scope 重写 | 纯文档 | 跟随各 wave |
| F107 | `ExecutionAdapter` trait 抽离 + `BgSpawner` 改造对接 | 重构(零行为变化)| Wave 1 |
| F108 | `TmuxInteractiveAdapter` 实现(模式 3 执行 runtime)| 新增 | Wave 2 |
| F109 | `ccteam-im-bridge` crate(模式 3 IM 触发源,TG 起步)| 新增 | Wave 2 |
| F110 | MCP namespace `ccteam` → `ct` + 子前缀工具命名 | Breaking | Wave 3 |
| F111 | MCP 工具粒度配置(`CCTEAM_DISABLE_TOOLS` + 项目级 `.mcp.json`)| 新增 | Wave 3 |

详 `prd.md`。

---

## 六、不在本版

- 模式 3 跨 session chat memory 重建(session-id 失效时,从 progress.jsonl 重放业务事件作 system prompt) — 延 V0.6.1
- IM bridge 多平台扩展(Slack / Discord) — 延 V0.7.x;V0.6.0 只落 TG + trait
- V0.5.x 延期 F98(plan-approval↔outbox 联动)— 见 `docs/v0-5-0/prd.md` 末段,本版仍不动
- mcp `ct` 之外的拆 server(多 server / standalone server)— 见决策记录 §七,本版不拆

---

## 七、决策记录(为什么这么做)

| 决策 | 选项 A | 选项 B | 选了 | 理由 |
|---|---|---|---|---|
| 模式 3 执行 runtime | `claude --resume` + stream-json 长跑 | Agent SDK adapter 进程内 agent loop | **A** | 跟现有 tmux pane plumbing 顺,Claude Code 全栈(skills / hooks / MCP)零损耗 |
| 模式 3 input 面 | send-keys | stdin pipe(`--input-format stream-json`)| **send-keys** | 复用 tmux 已有基础设施;stdin 模式将来可加 |
| 模式 3 output 面 | parse session jsonl | parse tmux capture-pane | **session jsonl** | 守"不解析 tmux 输出"红线;Anthropic 官方格式比 ANSI 文本稳定 |
| MCP server 拆 | 单 server `ct` + 子前缀 | 拆 2 server(`ct` + `ct_chat`)| **单 server** | `.mcp.json` 单条目;`CCTEAM_DISABLE_TOOLS` 抵消 tool list 长度风险 |
| MCP namespace 长度 | `t`(1 字母)| `ct`(2 字母)| **`ct`** | `t` 容易撞别人(OMC 已占);`ct` 是"ccteam"缩 |
| Breaking vs alias | 留 `ccteam__*` alias 一版 | 直接 breaking | **直接 breaking** | pre-v1.0 + CLAUDE.md §五 no shim |
| 模式 3 落地深度 | trait + adapter stub | trait + adapter + TG bridge 全栈 | **全栈** | trait + 一个真实 adapter 才能验证 trait 设计;TG 是最低复杂度 IM 选择 |
