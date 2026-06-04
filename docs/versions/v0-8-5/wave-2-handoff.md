# v0.8.5 Wave 2 handoff —— Claude 命令面(D5 四通道 gate + D6 AskUserQuestion)

> 范围:prd §3-D5/D6 + arch-refactor §6-W2。第 0 步真 claude smoke gating → **PASS → 主路**。
> 分支:`v0.8.5-w2`(off `origin/dev` = W1 的 b3ea112)。落地:gate 过后直接 commit dev + SSH 推(无 PR)。

## Decided

1. **第 0 步 §5.1 真 claude smoke = PASS(全 4 项)** on claude **2.1.162**(workflow 双路:live binary + 重建源码交叉核对)。① `/model <id>` 带参直接应用不弹窗、bare `/model` 弹 picker ② `/compact` BRIDGE_SAFE 原生内联透传 ③ 版本 2.1.162 ④【最关键】PreToolUse hook 返回 `allow + updatedInput.answers` → AskUserQuestion picker **被跳过**、答案直进 tool_result,pane + transcript jsonl 双证,且在 `--dangerously-skip-permissions` 下成立(D5/D6 运行模式)。⇒ **D5/D6 走主路,非 fallback**。
2. **D5 `ClaudeTuiAdapter::handle_directive` 四通道 gate**(`claude_tui.rs`):
   - 通道 1(prompt 开放集:skill/自定义/plugin)+ 通道 2(BRIDGE_SAFE local:compact/clear/usage/…)+ 未知 → 零知识透传 `submit_turn(UserText("/…"))` → Turn(开放集红线不变)。
   - 通道 3 arg-applicable popup(`model`/`effort`):带 args 或重入 choice → 直接 arg-form 透传(smoke 证实不弹窗);**bare → NeedsChoice**(model 选项 = MODEL_ALIASES;effort = low/medium/high);**绝不盲发 bare 弹窗**。
   - 通道 3 panel-only popup(curated:config/agents/permissions/mcp/hooks/plan/…)→ Rejected(无 chat-可驱动 arg 形态)。
   - `/esc` → send Escape(`ProcessBackend::send_escape` 默认实现 = 发 ESC 字节 `\x1b` 经 send_text,写不读;逃生舱)。
3. **D6 token-based External ingress**(must_fix #1 的 shared-registry 路径):
   - daemon 在 startup 建**一个**共享 `Arc<Mutex<PendingInteractions>>` → `gateway.set_pending(shared)` + 传 `serve_mcp_socket`/`handle_mcp_socket_connection`(`DaemonArgs.pending`)。
   - mcp.sock 新 op `interaction/ask {slug,role,question,options,multi}`:mint 唯一 token `h{nanos}` → 建 `ChoicePrompt` → resolve_home_chat → 注册 `External{oneshot}`(锁内)→ **drop guard** → 经 sink 发 GatewayEvent(带 options 按钮)→ **锁外** `timeout(600s, oneshot).await` → answer 回 hook(超时 `{timeout:true}` + take_by_token 清理)。
   - `intercept_ask.rs` chat 变体:读 AskUserQuestion stdin → slug/role(stdin 优先、env 兜底)→ mcp.sock(`ccteam_core::daemon_socket_path`,阻塞 UnixStream)→ answer → stdout `allow + updatedInput.answers`;失败/超时/无 slug → 降级 bg `deny`(老牌行为)。
   - `ensure_chat_hooks_installed` 追加第二个 PreToolUse 条目 matcher `"AskUserQuestion"` → intercept-ask wrapper。
   - **resolve_selection 改 token-global**(`take_by_token`):一条 callback 路径统一处理 Directive + External origin;**所有 producer 的 token 必须唯一**(D5 `claude_popup_prompt` 改 mint `cj{nanos}`,External `h{nanos}`,review 修)。

## Rejected / 偏离(明示,handoff 留痕)

1. **D5 local-jsx 名单是 curated 子集,非全 57**。references 有 ~57 个 local-jsx,但多数是即时渲染(help/usage/diff…非阻塞)。gate 只拦**明确阻塞型 picker/panel**(panel 名单 ~16 + arg popup model/effort);长尾 local-jsx 走透传 + `/esc` 兜底。`绝不盲发 bare 弹窗`对已知阻塞型成立;未列入的阻塞型靠 `/esc` 恢复。名单 drift-prone,bump claude 参考时重核。
2. **D6(D6-agent 实现,均已 review 核可的合理偏离)**:① slug/role 取 stdin 字段优先再 env(warm daemon HTTP fast-path 把 slug/role 折进 stdin 而非 env)② AskUserQuestion hook 命令恒 `{hook_sh} intercept-ask`(不走 W6 `mux hook-emit`——后者 fire-and-forget 无 stdout decision,而 AskUserQuestion 是阻塞决策 hook)③ HTTP fast-path 下 hook 会阻塞一个 axum worker 至多 TTL(follow-up:spawn_blocking)④ 多问题只处理第一个(follow-up)⑤ External 仅按钮点击(token)可解,bare 数字短回复不解(numeric 仍 (chat,session)-keyed,Directive 专用;follow-up)。
3. **resolve_selection 从 (chat,session)+token 改 token-global**:统一 Directive/External callback。代价:所有 token 必须唯一(已修)。numeric 路径仍 (chat,session)-keyed(Directive)。

## Risks

- **D6 live 端到端未自动验**:真 daemon + 真 claude pane 触发 AskUserQuestion → socket → IM → 回答的全链路无自动化测试(环境所限);§8-8 e2e 测的是 gateway 半边(无 socket)。socket client + daemon handler 单编译 + 逐条推理过,但未对活 `UnixListener` 跑。**建议 W4 或专机真机 smoke 补**。
- HTTP-fast-path worker 阻塞(见偏离 2③)。
- curated panel 名单可能漏一个阻塞型 popup(`/esc` 兜底)。

## Files

- `ccteam-harness/src/execution/claude_tui.rs` —— D5 四通道 gate + 4 const 名单 + `claude_popup_prompt`(唯一 token)+ `ensure_chat_hooks_installed` 追加 AskUserQuestion matcher。
- `ccteam-harness/src/lib.rs` —— `ProcessBackend::send_escape` 默认实现(ESC 字节经 send_text)。
- `ccteam-im/src/pending.rs` —— `take_by_token` / `prompt_by_token`。
- `ccteam-im/src/gateway.rs` —— `resolve_selection` token-global;3 个 D6 e2e 测试。
- `ccteam-im/src/daemon.rs` —— `DaemonArgs.pending` + 启动注入 `set_pending`。
- `ccteam-cli/src/main.rs` —— 共享 registry 装配 + `interaction/ask` op(`is_interaction_ask_call`/`execute_interaction_ask`)+ intercept-ask 读 stdin。
- `ccteam-hooks/src/intercept_ask.rs` + `lib.rs` —— chat 变体 + dispatch。
- `ccteam-harness/tests/claude_tui_test.rs` —— D5 通道分类测试(bare model/effort→NeedsChoice、panel→Rejected、token 唯一)+ D6 matcher 断言。
- `ccteam-im/tests/inbound_wiring_test.rs` —— `DaemonArgs { pending: None }`。

## Remaining(交下游)

- **W3(Codex)**:F10 + D2/D2.4/D4 + 起手 `git -C references/codex checkout b2344d8`。
- **W4**:P3 `thread_status` 两端实现 + 渲染;P1 register_commands/setMyCommands;**D6 真机 live smoke**(本 wave 未自动验)。
- **D6 follow-ups**:多问题、External numeric-reply、HTTP-fast-path spawn_blocking。
- **D5 follow-up**:local-jsx 名单 drift 防护(类似 W3 codex 67-枚举快照测试)。

## Gate(push 前实测)

- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = **1865 / 0**(W1 后基线 1858,+7:D5 通道分类 ×3 + D6 e2e ×3 + matcher ×1)。
- `cargo clippy --workspace --all-targets -- -D warnings` = **0**(ccteam-web 本 wave 未改,沿 W1 clean)。
- `cargo fmt --all --check` = **clean**。
