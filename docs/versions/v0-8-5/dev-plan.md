# v0.8.5 dev-plan —— 执行编排(wave-based)

> 配套 `prd.md`(实现规约)+ `docs/research/im-slash-coverage-decision.md`(设计论证)。本文只讲**怎么把它落地**:wave 切分、依赖、gate、收尾。架构与验收以 `prd.md` 为准。
> 这版大(动 `ccteam-harness` / `ccteam-im` / `ccteam-hooks` 多 crate + 新 trait + hook + 两 vendor adapter),按 minor 版 4-wave 范式走。

## Wave 依赖图

```
Wave 1 中立基建        Wave 2 Claude            Wave 3 Codex             Wave 4 收尾
D1 handle_directive ──┬─► D5 四通道 gate      ┬─► D2 全量映射 + D2.4    ─► P1 菜单+/help
+ 中立类型            │   (smoke 第0步 gate)  │   tokenUsage 消费       ─► P3 /sessions(C+X)
+ gateway 纯路由      │   D6 AskUserQuestion  │   D4 弹窗两段式          ─► P4 .agents/plugins
+ 删 SystemDirective  │                       │                          ─► ship-gate
D3 ChoicePrompt 传输 ─┘                       └─ (drift 快照测试)
+ tg callback_query
```

- **Wave 1 是地基**:D5/D2 的 outcome 渲染、D6 的 ChoicePrompt 都依赖 D1 + D3。先合。
- **Wave 2、3 都 dep Wave 1**,彼此独立(Claude vs Codex adapter 不交叉),可并行起 worktree;若都动 `transport/mod.rs` 同段则串行更稳。
- **P1 / P3-Claude 与命令面正交**,可在任意 wave 并行起;放 Wave 4 统一收(P3-Codex dep Wave 3 的 D2.4)。

## 各 wave 固定动作

1. **起 worktree**:`git worktree add -b v0.8.5-wN /tmp/ccteam-v085-wN origin/dev`(从 dev 起)。
2. **起手对版**:`git -C references/codex log -1` 应见 `b2344d8`;按 prd §7 复核引用没漂(尤其 Codex RPC method 名 + Claude 重建版语义)。
3. **实现 + deterministic 测试**(fake adapter / MockChannel / scripted JSON-RPC peer / fixture;**不**依赖真 token,除 Wave 2 第 0 步的真 claude smoke)。
4. **gate**(prd §4.5):`cargo test --workspace --exclude ccteam-web` ≥ 1800/0(上版基线)+ clippy `-D warnings` 0 + `cargo fmt --all --check`。退步不发 PR。
5. **PR**:描述映射 `requirements.md`(IM 日常驱动)+ `prd.md` 对应 D/P + 列 AC 勾选 + 五段 handoff(Decided / Rejected / Risks / Files / Remaining)。
6. **review/fix/merge** 后 `git worktree remove`。

---

## Wave 1 —— 中立基建(D1 + D3)

**目标**:命令面的中立骨架立起来,gateway 退纯路由,ChoicePrompt 能渲染回填(先用 FakeAdapter 驱动,不依赖任何真 vendor)。

落点:
- `ccteam-harness`:trait `handle_directive`(无 default)+ 中立类型 `Directive`/`DirectiveOutcome`/`ChoicePrompt`/`ChoiceOption`/`ChoiceSelection`(与 `ThreadEvent`/`ApprovalIR` 同层)。
- `ccteam-im/transport`:`SendMessage.options: Vec<ChoiceOption>`(`#[serde(default)]`)+ 各 provider 渲染骨架:Telegram inline keyboard **+ `callback_query` 入站新路径** / web chat WS chips / 兜底纯文本编号 / mock。
- `ccteam-im/gateway`:slash 文本→`Directive`→`handle_directive`→outcome 渲染;**pending-picker**(keyed by chat+session,TTL,单飞)+ 选择重入;数字短回复 / 完整 arg 形态归一为 `ChoiceSelection`。删 `turn_input_for_session` vendor 分支 + `compact|review` allowlist + `TurnInput::SystemDirective`(全量 grep caller 迁移,不留 alias)。gateway 自有命令集不变。
- `FakeAdapter` 补 `handle_directive`;`codex_exec.rs`(bg)`handle_directive` 全拒。

**验收 gate**:
- FakeAdapter 产 5 种 `DirectiveOutcome` → gateway 各自正确渲染回 IM(Turn/Done/Rejected/Redirect/NeedsChoice)。
- `NeedsChoice` → IM 出选项 → 三形态回填(按钮回调 / 数字 / arg-form)都归一为同一 `ChoiceSelection` → 重入。
- 解耦自检:写一个假 channel,只实现 options 渲染 + 回填归一 → 锁住「channel 不感知 vendor」边界。
- `TurnInput::SystemDirective` 全仓 0 引用(grep)。
- baseline 不退。

## Wave 2 —— Claude 命令面(D5 + D6)

**⚠️ 第 0 步(gating,不可跳)**:跑 prd §5.1 的真 `claude` binary smoke(4 项,~10–15min)。
- **PASS** → 按 D5/D6 设计实现。
- **FAIL** → D6 退 deny-with-reason fallback;D5 退「local-jsx 一律 Rejected + 指引」。在 handoff 记录实测结果 + 选了哪支。

落点(smoke PASS 前提):
- `claude_tui.rs`:`handle_directive` 四通道 gate —— prompt 透传 / `BRIDGE_SAFE` local 集透传 / local-jsx(带 args 直通、bare→`NeedsChoice`→arg-form send-keys 改写、无 arg 等价的面板→Rejected/Redirect)/ agent 弹窗→D6;`/esc`→send-keys Escape。**绝不盲发 bare 弹窗**。
- `ccteam-hooks` `intercept_ask.rs`:chat 变体(截 AskUserQuestion → daemon mcp.sock → 构造中立 ChoicePrompt(producer=hook)→ IM → 阻塞等答案 → stdout `allow + updatedInput.answers`);超时 deny-with-reason。
- `claude_tui.rs` `ensure_chat_hooks_installed`:加 AskUserQuestion matcher(现仅装 chat-progress)。

**验收 gate**:
- 真 binary smoke 4 项过(或记录 fallback)。
- 通道分类单测:prompt/local/local-jsx 各走对路径;bare 弹窗产 `NeedsChoice`、绝不直发。
- local-jsx 名单有「从 `references/claude-code/src/commands/` 同步」的来源注释。
- D6:fake AskUserQuestion 链路 → ChoicePrompt 出 IM → 回答 → `updatedInput` 回填(或 fallback 路径);超时 deny。
- baseline 不退。

## Wave 3 —— Codex 命令面(F10 前置 + D2 + D2.4 + D4)

**⚠️ 第 0 步(gating,前置,不可跳)**:F10 fix A —— codex chat 默认 transport 改 **stdio**(prd §3-F10)。
- 改 `codex_app_server.rs::client()`:无 socket override + 无 transport override → `connect_stdio_command`;UDS 仅显式 `CCTEAM_CODEX_APP_SERVER_SOCKET` 时启用。UDS 代码保留。
- 补测试:`daemon.rs:1137` 加「默认无 env 选 stdio transport」断言。
- **验**:真机 `/new codex <handle>` 能 spawn 出能连的 session(对 PATH `codex`)。
- **为何前置**:不修则 D2/D4/P3-Codex 全无可测对象(同 Wave 2 第 0 步真-claude smoke)。fix B(起 codex daemon + ws-over-UDS 握手)**弃**。

落点(F10 通过后):
- `codex_app_server.rs`:`submit_system_directive` → `handle_directive` 三层 resolution(内建表 → skills/list 动态 → Rejected）+ 六类映射表(prd §3-D2.1,锚 b2344d8 RPC)+ active-turn map(`/interrupt` + `turn/steer{expectedTurnId}`)+ per-session override map(照 `bridges` 先例 keyed by thread_id)。
- **D2.4(独立工作项,非 freebie)**:`events()` 流订阅 `thread/tokenUsage/updated` → per-thread token 缓存 → 喂 `/status` 与 P3-Codex。**先确认 ccteam 现确实未消费**该通知,再接。
- D4:bare 弹窗(model/collab/personality/permissions/review/skills/memories/resume)两段式 `NeedsChoice`;带 args 直 apply。
- **drift 快照测试**:pin Codex `SlashCommand` 67 枚举名常量 list,断言「内建表 + reject 名单」覆盖之;新命令未分类报错。**不**做 codex crate runtime 依赖。

**验收 gate**:
- scripted JSON-RPC peer 逐 RPC arm:compact/review(4 target)/interrupt/fork/rollback/rename/goal/stop/memories/diff/init + 查询合成(status/model/skills/mcp/hooks/apps)+ override(model/personality/collab/permissions)各 arm 过。
- skill resolution:`skills/list` 夹具 → 命中走 `turn/start{Skill}`;`skills/changed` 失效缓存;miss → Rejected 附候选。
- D2.4:tokenUsage 通知 → 缓存更新 → `/status` 含 token;断言 ccteam 确实订阅(非假设)。
- `/clear` 对 codex 从 UserText echo 改 `Redirect`(更新既有 dual-vendor 测试)。
- drift 快照测试覆盖全 67;TUI-only 一刀切 Rejected。
- baseline 不退。

## Wave 4 —— 收尾(P1 + P3 + P4 + ship-gate)

落点:
- **P1**:`telegram.rs` startup `setMyCommands`(gateway 自有命令 + `/help`);`/help` 实现;透传 slash 不进菜单。
- **P3**:session 状态属性 + `/sessions` 渲染(绝对值+%)。Claude adapter:transcript usage 求和 + model + `[1m]` 后缀读窗口 + 200k 兜底(不写死 per-model 清单)。Codex adapter:消费 Wave 3 的 tokenUsage 缓存。
- **P4**:补 `.agents/plugins/marketplace.json`(Codex 市场清单)+ 真机验 `codex plugin marketplace add` 装上 + `skills/list` 见 ccteam 7 skill。
- **ship-gate(prd §6)**:`Cargo.toml` `0.8.4→0.8.5`、`CLAUDE.md §一` baseline 回填、`tech-design.md`(handle_directive/ChoicePrompt 架构 + 协议→代码指针表)、`README.md`(英)+ `docs/usage.md`、各 wave handoff 五段、`ccteam doctor --verify-mcp` 0 drift。

**验收 gate**:
- P1:菜单真机出现(零参数命令);`/help` 列对。
- P3:fake transcript(带/不带 [1m])→ ctx% 算对、显示 `188k / 1M (19%)` 形态;Codex stub/真都不崩。
- P4:`codex plugin marketplace add` 端到端真机通(或记录阻塞)。
- 全套 ship-gate 文档同步;clippy 0 + fmt + baseline。

---

## 踩坑备忘

- env-mutating 测试放 `crates/*/tests/*.rs` integration(独立进程),不放 lib `#[cfg(test)]`。
- 改 `Channel`/`SendMessage`/`ChannelMessage` 公共结构 → grep 全 impl(tg/slack/discord/mock/ws)+ 全 caller;新字段 `#[serde(default)]`。
- ledger 断言 multiset + pairing,非 positional(防 v8.2 race flake)。
- `gh auth token` 无 workflow scope → 改 `.github/workflows/*` 用 SSH 推;本版大概率不碰。
- `ccteam-web` ws_* 测试留 CI/专机,本机 baseline 用 `--exclude ccteam-web`。
- WSL/inotify-busy 宿主 watcher/SSE 502 = 环境层,不计 baseline。
