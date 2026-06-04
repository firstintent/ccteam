# v0.8.5 Wave 4 handoff —— 收尾(P1 菜单 + P3 /sessions 状态 + P4 skill 双装 + ship-gate)

> 范围:prd §3-P1/P3/P4 + §6 ship-gate。最后一 wave:补 IM 运维三件 + 发版。
> 分支:`v0.8.5-w4`(off `origin/dev` = W3 的 9277c4d)。落地:ship-gate 过后 commit dev + SSH 推 + tag。

## Decided

1. **P1 菜单**(`daemon.rs` + `gateway.rs` + `telegram.rs`):daemon 启动 `build_channels` 后 `for ch in channels { ch.register_commands(menu_command_specs()) }`(warn-on-fail);`menu_command_specs()` = `GATEWAY_COMMANDS` 里 `in_menu==true` → `CommandSpec`(`/new /sessions /projects /help`;透传 vendor slash 不进菜单)。`TelegramChannel::register_commands` override → `setMyCommands`(bare 名,照 `api_url` POST);其余 channel 默认 no-op。
2. **P3 /sessions 状态**(`adapter.rs` + `transcript_tail.rs` + `claude_tui.rs` + `gateway.rs`):单点渲染 helper(`format_tokens`/`ContextUsage::render`/`ThreadStatus::status_suffix`)产 `188k / 1M (19%)`;`/sessions` 与 Codex `/status` **同源**(都经该 helper)。Claude `thread_status` 倒读 transcript **尾 512KB**(不全 parse)取末条 `message.usage`(input+cache_creation+cache_read)+ `message.model`;窗口 = model 带 `[1m]` ⇒1M 否则 **200k 基线**(唯一常量,无 per-model 清单)。Codex `thread_status` 读 `CodexThreadTracker`(W3 已就位)。
3. **P4 skill 双装**(`.agents/plugins/marketplace.json` 新增):Codex **市场清单**(`marketplace_add` 只读它)。schema 锚 b2344d8 `core-plugins`:`{name, interface{displayName}, plugins:[{name, source:{source:"url", url:"firstintent/ccteam"}, category}]}`,url-source-at-root 解析到含 `.codex-plugin/plugin.json`(声明 `skills:"./skills/"`)的仓根。**真机验**(codex 0.136.0,隔离 CODEX_HOME):`codex plugin marketplace add` 成功、`plugin list` 见 `ccteam@ccteam`;完整 install + 7 skill 发现对**本地 clone** 验通(公网 URL 装受沙箱 cloneability 限)。
4. **ship-gate**:`Cargo.toml` + 4 个 plugin 清单(`.claude-plugin/{plugin,marketplace}.json`、`.codex-plugin/plugin.json`)+ `Cargo.lock` `0.8.4→0.8.5`(`plugin_manifest_version_test` 守版本一致性);`CLAUDE.md §一` baseline `1903/0` + 当前在做改 v0.8.5;`tech-design.md`(命令面架构 + 协议→代码指针表);`README.md`(英,无版本进展)+ `docs/usage.md`(菜单/help、各 slash 在 IM 的行为、/sessions 字段、skill 双装);`ccteam doctor --verify-mcp` = **PASS(28 工具 0 stub 0 drift)**;各 wave handoff 五段。

## Rejected / 偏离

1. **P4 用 git **url** source 而非 local** —— Codex 拒绝 local `"./"` 根 source(plugin 清单在仓根非子目录);url-source-at-root 是正确的自发布形态,已端到端验。清单用 github 简写 `firstintent/ccteam`(对齐 PRD 的 `codex plugin marketplace add firstintent/ccteam`);公网 URL 装仅受本沙箱 repo 不可 clone 限。
2. **P4 `codex skills list` 无独立 CLI 子命令**(codex 0.136.0)—— skill 运行时经 app-server `skills/list` RPC 暴露(读 installed skills dir),已验该 dir 含 7 skill。
3. 累计 W2/W3 的真机未验项(D6 live 全链路、Codex EXPERIMENTAL collab/permissions wire 形态)沿前 handoff,列 post-ship follow-up(用户已知并裁定 ship)。

## Risks

- P3 Claude `thread_status` 真机未对真 transcript 形态逐字段验(单测用 fake transcript);倒读 512KB 尾窗对超长单行 usage 行的极端情形未压测。
- 见 W2/W3 handoff 的 live-verification follow-up(用户裁定 post-ship)。

## Files

- `ccteam-harness/src/adapter.rs` —— `format_tokens`/`ContextUsage::render`/`ThreadStatus::status_suffix` + 测试。
- `ccteam-harness/src/execution/transcript_tail.rs` —— `read_status_tail`/`parse_status_row`/`context_window_for_model` + 200k/1M 常量。
- `ccteam-harness/src/execution/claude_tui.rs` —— P3 `thread_status` 实现 + `resolve_transcript_path`。
- `ccteam-harness/src/execution/codex_app_server.rs` —— `/status` 经 `ContextUsage::render`。
- `ccteam-harness/src/lib.rs` —— re-export `format_tokens`。
- `ccteam-im/src/gateway.rs` —— `menu_command_specs()` + `render_sessions` async 加 model/ctx + FakeAdapter `set_status`。
- `ccteam-im/src/daemon.rs` —— P1 startup 注册。
- `ccteam-im/src/transport/providers/{telegram,mock}.rs` —— `register_commands` override / 记录。
- `ccteam-im/tests/daemon_test.rs` —— P1 startup 菜单注册测试。
- `.agents/plugins/marketplace.json` —— **新增**(P4 Codex 市场清单)。
- ship-gate:`Cargo.toml` + `Cargo.lock`、`.claude-plugin/{plugin,marketplace}.json`、`.codex-plugin/plugin.json`、`CLAUDE.md`、`docs/tech-design.md`、`README.md`、`docs/usage.md`。

## Remaining(v0.8.5 之后)

- 真机 live smoke:D6 hook→IM 全链路、Codex EXPERIMENTAL collab/permissions/login、多 session 单子进程、`/fork` 自动注册(gateway 变体)。
- PRD §3-D2.5 「67」更正为 53(已在 W3 handoff 记)。
- web 出站文件 attachments;多问题 AskUserQuestion;External numeric-reply。(D6 HTTP-fast-path spawn_blocking 已在下方 post-review S1 落地)

## Gate(push 前实测)

- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = **1903 / 0**(W3 基线 1890;P1/P3/P4 + plugin 版本一致性测试 → 1903)。
- `cargo clippy --workspace --all-targets --locked -- -D warnings` = **0 warnings**(干净)。
- `cargo fmt --all --check` = **clean**。
- `ccteam doctor --verify-mcp` = **PASS**(28 工具 / 0 stub / 0 drift)。

> ship-gate 实测捕获:`Cargo.toml` 版本 bump 后,4 个 plugin 清单的 `version` 字段仍是旧值 → `plugin_manifests_match_workspace_version` 红测;同步 bump 4 清单 + `cargo update --workspace` 刷新 `Cargo.lock`(否则 `--locked` 拒跑)后全绿。

## Post-review fixes(dev,用户 review 后 — B1 blocker + S1–S4)

用户 review v0.8.5 dev 分支裁定 **HOLD**,直接在 dev 修真问题(无 PR、tag 仍 held):

1. **B1(blocker)**:`daemon.rs` 入站 consumer 先 `sec.evaluate(&content)`;按钮/chip 回调 `content=""` → `EmptyAfterSanitize` → 在到 `handle_message` 前被 drop,所有 D3 选项按钮 + D6 AskUserQuestion 按钮在真 daemon 失效(Telegram + web chat 都经此 consumer)。修:抽 `sec_gate_payload(outcome, has_selection)` —— 仅当带 selection 时放行 `EmptyAfterSanitize`(ACL/限流/签名拒绝在 sanitize 之前,照旧 drop,选择回调不能绕过)。
2. **S1**:暖 daemon HTTP fast-path(`ccteam-web internal_hook`)async handler 直接调阻塞 `ccteam_hooks::dispatch`(intercept-ask 等 mcp.sock 至多 660s)→ 饿死单线程 daemon runtime,写答案的 mcp.sock task 排不上 → D6 自死锁至超时。修:`dispatch` 改 async + `spawn_blocking`(照 W6 hook-sink)。冷 CLI 子进程路径不变。
3. **S2**:Directive pending TTL 设了不生效(`drain_expired` 0 production caller;resolve 路径忽略 `expires_at`)→ 过期 prompt 的迟到 click/数字仍重入 dispatch。修:`resolve_selection`/`resolve_numeric`/`has_pending_for_current` 持锁内先 `drain_expired(now)`(同时清掉过期 External 的 oneshot → hook deny,和其 tokio timeout 同义)。
4. **S3**:`read_status_tail` seek 到 `size−512KB` 后 `read_to_string` 撞多字节 UTF-8 边界(CJK/emoji)→ `InvalidData` → /sessions ctx 后缀丢(仅 Claude 该 session,非硬失败)。修:读 `Vec<u8>` + `from_utf8_lossy`(partial 首行照旧丢,替换符不入解析)。
5. **S4**:全 workspace 0 个非 None selection 的 daemon 级测试(B1 能 ship 绿之因)。补:`sec_gate_payload` 单测 + 真 `ThreeLayerSec` 选择回调测(daemon.rs)、过期 pending drain 测(gateway.rs)、>512KB UTF-8 边界切分测(transcript_tail.rs)。

> 修后 gate:`cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = **1907 / 0**(1903 + 4 回归测);`cargo clippy --workspace --all-targets --locked -- -D warnings` = **0**;`cargo fmt --all --check` = **clean**。
> 死路 `inbound.rs::process_inbound`(orchestrator-era,daemon 不调、仅测试引用)未动 —— review 未点名、不扩 scope。
> review 提及「1903/0 不稳定可复现」:本机 WSL2/inotify 触顶会致 watcher/SSE/web e2e 偶发 502(CLAUDE.md §六 已记,属环境层,不计 baseline);canonical 命令本身确定性 1907/0。
