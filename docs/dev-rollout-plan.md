# ccteam 文档批量改动 rollout 计划

> **开发态滚动文档**——完成后归档(移到 `docs/archive/` 或删除),不进 ccteam 产品。
>
> 用途:把已达成共识但未落地的 5 条架构决定批量改进 5 份产品文档。
>
> **任何 fresh session 读本文都能接力**——不依赖对话历史。

---

## 1. 已固化的架构决定(subagent context 来源)

下列 5 条决定已在前期讨论中固化,**任何阶段执行都按这些办,不重新讨论**。

### D1. 实现语言 = Rust 单 binary

- `ccteam` = 单一 Rust binary(orchestrator daemon + CLI + hooks 子命令)
- Cargo workspace:`crates/{ccteam-core, ccteam-cli, ccteam-hooks}`
- hook 调用:`ccteam hook progress-append` / `ccteam hook cost-accumulate` 等(不再独立 bash/python)
- official plugin 的 hook(如 `security_reminder_hook.py`)通过 shell shim 包装挂上,不直接依赖
- 默认 dep:tokio / clap derive / serde + serde_json / notify(inotify)/ tracing
- 分发:brew + install.sh + cargo install,**零运行时依赖**(Rust 静态链接)

### D2. 前端层 = 可插拔多前端,核心 headless

- ccteam 核心(orchestrator + tmux + hooks)是 headless 状态引擎,前端可插拔
- M0:CLI + tmux 三 pane 布局
- M3+(机会主义,非关键路径):`ccteam tui` ratatui 本地交互式仪表盘
- M4+(机会主义,非关键路径):`ccteam serve` web dashboard(xterm.js + WebSocket bridge)
- 三种前端共用 `ccteam-core` lib API → **crate 切分从 M0 起严格 lib/binary 分离**

### D3. 前端层 invariant(红线)

任何前端(CLI / TUI / web dashboard)**不得**在 ccteam 内引入新 LLM 层。

- ✅ web dashboard 通过 xterm.js + WebSocket 桥**直通到 tmux 内的项目级 claude**——等价于"远程版 `ccteam attach`"。用户在浏览器键入 = 通过 send-keys 注入 tmux,不经任何 ccteam 中介 LLM
- ✅ web 介入触发 PreToolUse hook 检测 user_attach,自动暂停 phase(与本地 attach 语义一致)
- ❌ 不在 ccteam 层起 meta-claude / 自实现聊天 UI / 翻译用户 prompt(已被否决的 `ccteam chat` 路径复活)

LLM 推理只发生在两处:① L2 项目级 claude(tmux 内) ② L0 用户自带 claude(机器上的 `claude` 进程)。

### D4. 用户对话入口 = 用户自带 claude session(已落地于上一轮)

- M0:CLI 必须 `--format json`
- M1:出 `~/.claude/skills/ccteam-control/SKILL.md`
- M2+:出 `ccteam-mcp` MCP server

### D5. 参考实现指针 = `references/agent-of-empires/`

- 已 clone 到 `~/workplace/agents/ccteam/references/agent-of-empires/`,`.gitignore` 屏蔽不入仓库
- 栈与 ccteam 完全对齐:Rust + ratatui + crossterm + tokio + axum(ws)
- M3 TUI / M4 web dashboard **抄前端层**:Cargo.toml dep 组合 + WebSocket bridge 范式 + 它的 `docs/guides/web-dashboard.md`
- **不抄核心**:9-phase 编排、Seed Gate、跨项目 RAG、Defense in Depth 是 ccteam 差异化护城河,AoE 没有

---

## 2. 5 阶段任务

A 必须先(tech-design 是真理源头);B/C/D/E 之间无依赖,但顺序跑更稳(主 session context 不爆)。

### Stage A — `docs/tech-design.md`

**改动**:

1. §3.8 末尾追加"前端层(可插拔)"小节:
   - 画前端层 → `ccteam-core` → orchestrator 分层关系
   - 明确 Warp = 用户终端选择(ccteam 透明兼容,**不**是 ccteam 集成对象)
   - ratatui = M3+ 可选 TUI;xterm.js + WS bridge = M4+ web
   - 写**前端层 invariant 红线**(D3 全文)
   - 引用 `references/agent-of-empires/` 作为 M3/M4 抄作业指针
2. §6.1 加一句:"orchestrator 的 Rust 实现用 `tokio::process::Command` 包装这些 tmux 命令"
3. §6.2 hook 论证段末尾追加:"hook 实现是 `ccteam hook <name>` 子命令——单 binary 分发,与 orchestrator 共享 serde schema"
4. §6.4 ccteam-mcp 段末尾追加:"`ccteam-mcp` 与 `ccteam-core` 同 crate,通过 `mcp-serve` 子命令暴露——为将来 `ccteam tui` / `ccteam serve` web 前端预留同一状态读写 API"

**验收**:§3.8 出现"前端层 invariant" + `references/agent-of-empires/`;§6.1 含 "tokio";§6.2 含 "`ccteam hook`"

### Stage B — `docs/interfaces.md`

**改动**:

1. §6.1 项目级 `settings.json` 模板:hook command 全部改 `"ccteam hook <name>"`(不再 `"scripts/<name>.sh"`);加注释说明 D1
2. §6.3 标题 `cost-accumulate.sh` → `cost-accumulate` 子命令;实现描述从 Python 改为 Rust(serde_json 解析 `usage.*` 字段累加);保留"Claude Code 不在 hook 输入里给 cost_usd"关键事实
3. §10.1 加 `ccteam mcp-serve` 行(M2+);§10.6 加 `ccteam hook <subcmd>` 行(debug 用)
4. 新增 §14 `ccteam-core` lib API 草案(M0 占位):
   - 列 ≥5 条 API 签名(`get_state(slug)` / `list_projects()` / `submit_control(slug, signal)` / `tail_progress(slug, n)` / `attach_progress(slug) -> Stream`)
   - 标注:"M0 起以 lib crate 提供,内部 unstable;M3 ratatui TUI 上线时定为 1.0"

**验收**:§6.1 所有 hook 以 `ccteam hook` 开头;§6.3 标题不带 `.sh`;§14 存在签名 ≥5 条;章节顺序 §10→§11→§12→§13→§14

### Stage C — `docs/development-plan.md`

**改动**:

1. M0.1 仓库骨架:`pyproject.toml` → `Cargo.toml` workspace + 三 crate(`ccteam-core` / `ccteam-cli` / `ccteam-hooks`);验收 `cargo build --release` 出单 binary `ccteam`
2. M0.3:"3 个 hook 脚本" → "`ccteam hook` 子命令组,3 个子命令";实现位置 `crates/ccteam-cli`
3. M3 加 M3.X(机会主义,非关键路径):"ratatui TUI 前端" — 抄 `references/agent-of-empires/` 的 Cargo.toml dep 组合 + 主循环范式
4. M4 加 M4.X(机会主义,非关键路径):"web dashboard" — 抄 `references/agent-of-empires/` 的 axum ws + xterm.js bridge
5. §8 关键路径"非关键路径"列表加 M3.X / M4.X

**验收**:M0.1 含 "Cargo workspace" + 三 crate 名;M0.3 含 `ccteam hook` 术语;M3/M4 各 1 条机会主义任务标注"非关键路径";`references/agent-of-empires` 引用 ≥2 次

### Stage D — `docs/user-guide.md`

**改动**:

1. §1 前提:删 "可选 `python3`(成本累计 hook 用)";加 "**任何 tmux 兼容终端都能 attach**——推荐 [Warp](https://github.com/warpdotdev/warp) / iTerm2 / Alacritty 等做更好本地终端,ccteam 无需特殊集成"
2. §2 装:加 "binary 5–10MB,**单文件、零运行时依赖**(Rust 静态链接)"
3. §3 init 体检:删 `python3` 行
4. §11 FAQ 加:"Q: ccteam 跟 Warp / iTerm2 怎么集成?A: 不需要集成。Warp 等是用户终端选择,`ccteam attach` 在任何 tmux 兼容终端里行为完全一致。"
5. **不**加 TUI / web dashboard 章节(M3+ 才有,等真做了再补,避免承诺未做事)

**验收**:§1 / §3 不再出现 `python3`;§2 含 "零运行时依赖" / "Rust 静态链接";§11 FAQ 含 Warp 说明

### Stage E — `CLAUDE.md`

**约束**:总行数 < 250(当前 205,预算 45 行)。

**改动**:

1. §一 编排者行:"Python orchestrator(M0)" → "Rust orchestrator(M0,Cargo workspace)"
2. §3.7 Plugins / Marketplaces:"本机已缓存"列表前加一段:"**项目本地参考**:`references/agent-of-empires/`(Rust + ratatui + axum ws,M3+ TUI / M4+ web dashboard 抄作业;`.gitignore` 屏蔽不入仓)"
3. §四 "(M0 待建)" 块:`pyproject.toml` → `Cargo.toml`(workspace);`orchestrator/`(Python asyncio)→ `crates/ccteam-core/` + `crates/ccteam-cli/`;加 `references/` 行

**验收**:§一含 "Rust orchestrator";§3.7 含 `references/agent-of-empires/`;§四含 `Cargo.toml` workspace;`wc -l CLAUDE.md` < 250

---

## 3. 执行方式

主 session 用 `Agent` 工具顺序 spawn 5 个 general-purpose subagent。每个 subagent prompt 模板:

```
你是 ccteam 项目的开发 Claude 实例,执行 dev-rollout-plan §2 Stage <X>。

阅读:
1. /home/rob/workplace/agents/ccteam/docs/dev-rollout-plan.md(本计划,§1 决定 + §2 Stage <X>)
2. /home/rob/workplace/agents/ccteam/<目标文档>(要改的)

按 Stage <X> "改动"清单逐项修改目标文档,完成后用"验收"清单自检。

约束:
- 只改 Stage <X> 的目标文档,不动其他文件
- 不 commit——主 session 统一 commit
- 不要重新讨论已固化的决定(§1 D1–D5)

返回 <150 word summary:改了哪几节、有无跳过、验收是否全过。
```

每阶段完成后主 session:
- 抽样 `grep` 验证关键 marker 出现
- `TaskUpdate completed`
- 进入下一阶段

5 阶段全过 → 一次性 commit,本计划归档。

---

## 4. 不做的事(scope guard)

本次 rollout **不做**:
- 写任何 Rust 代码(M0.1+ 实现任务,不在文档批量改范围)
- 改 phase 模板(M0.2)/ 写 hook 实现(M0.3)
- 任何 `ccteam-core` / `ccteam-cli` / `ccteam-tui` / `ccteam-serve` 代码
- `docs/requirements.md` 改动(13 痛点不变)
- `docs/claude-code-best-practices.md` 改动(只读副本)

超出范围 → backlog,新 PLAN 文档,不混入本次。

---

## 5. 完成后状态

5 份产品文档共同体现:
- 实现语言:Rust 单 binary
- 前端层:CLI(M0)/ TUI(M3+ 机会主义)/ web dashboard(M4+ 机会主义)
- 前端层 invariant:不引入新 LLM 层(但远程透传 tmux 介入是允许的)
- 入口路径三档:CLI / 用户自带 claude / Telegram bot
- 参考指针:`references/agent-of-empires/` 是 Rust 前端栈抄作业源

之后可以正式启动 M0.1(Cargo workspace 骨架)。

> **完成后归档本文。**
