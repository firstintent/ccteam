# v0.8.6 Review-Fix Batch 3 (🟡 CLEANUP) Handoff

> worktree `v086-fix`(承 batch 1+2)。一提交 `v0.8.6 fix:`。baseline 1870 → **1861/0**(-9:#11 删 8 个 flex 测试 + #10 净 -1;均为已删功能的死测试,0 回归,落在 ≥1861 地板)。clippy 0 含 web,fmt 干净。ccteam-web 234 pass / 5 env-gated ws_*。

## #9 DEFAULT_WORKFLOW_YAML 仍声明 explorer
- 改:模板 `agents:` 由 explorer → **cto**(与种入的 `.claude/agents/cto.md` 一致);去 `docs/versions/v0-4-0` 文档路径引用 + `(V0.4.0+ shape)` 版本标签。模板保持最小+合法。

## #10 chat_reset 删不净 —— **adversarial 修正了 review 前提**
- review 说 `chat_reset_signal_path`(lib.rs:259)+ `chat_reset_signal_test.rs` 都是孤儿要删。**实证(我主核)**:
  - `chat_reset_signal_path` **确是孤儿**(零生产调用;只剩测试用;daemon.rs:321「no supervisor tick」;supervisor 用自己的路径 `bot_dir/signals/RESET_SIGNAL` 读,不经此 helper)→ **删除**(lib.rs)。
  - 但 `chat_reset_signal_test.rs` **不是孤儿** —— 它测的是 **live 的 BotSupervisor**(`decide()` / `reset_session()` / `apply_action`,7 个测试),只有路径 setup 用了该 helper。**误删则丢 supervisor 覆盖**。
- 修(正确做法):删 helper;测试文件**保留** —— 把 2 处 helper 调用(`write_reset_signal` + 一处 assert)改用 supervisor 自己的路径基 `bot_dir(pr,r).join("signals").join(RESET_SIGNAL)`(本就是 supervisor 读的路径,更贴切);1 个 const-lock 测试改写为只断言 `RESET_SIGNAL == "reset.signal"`;文件 **rename → `supervisor_reset_signal_test.rs`**(名实相符)+ 刷新头注释。`supervisor.rs` 本体未碰(deferred 自愈机制,非本 review 范围)。
- 净:删 helper + project_dir_resolve_test 里 1 个 helper 测试 = -1 测试(supervisor 7 测试全留)。

## #11 team.rs flex 残留
- 删:`TeamSpec.sessions` 字段 + `DefaultSessionSpec` struct + `kind: flex` doc 残留 + 8 个 `f47_sessions_*` round-trip 测试(grep 证零 live 非-flex 消费者;pre-v1.0 无 compat)。`HarnessKind` 保留(api_v1 在用)。team.rs -193 行 + lib.rs 去死 re-export。

## #12 auth CSRF doc 与实现不符
- 实证:cookie 是 **`SameSite=Strict`**(auth.rs:222)——对 mutating(POST/PUT/DELETE)跨站请求**足够**(Strict 全拦跨站)。同源 SPA 经 `fetchInterceptor.ts` 注 `Authorization: Bearer`。→ **改 doc 匹配现实**(SameSite-based CSRF + Bearer for API);**未**加冗余 header 强制(会破 cookie-only 导航/SSE,零安全收益)。

## #13 / #14
- #13:meta_agent.rs:134 `ccteam new` → `ccteam project new`(其余命令串已是新分组)。
- #14:main.rs LoadContext 注释改为「validating no-op SessionStart seam,不写 .ccteam/ready」(W2 已移除 writer)。

## D1.4(可选)—— SKIPPED
- `state/orchestrator.pid → daemon.pid` 未做:常量在 ccteam-core/daemon.rs(PIDFILE_NAME)+ graceful_shutdown_test 3 处 reader,wave-2-handoff 已记为推迟。低值、可选。若要做是个干净 rename(pre-v1.0 无 alias),随时可补。

## Files
- ccteam-core:team.rs(#11 -193)、meta_agent.rs(#13)、lib.rs(#11 re-export)。
- ccteam-cli:commands.rs(#9)、main.rs(#14)。
- ccteam-im:lib.rs(#10 删 helper)、tests/supervisor_reset_signal_test.rs(#10 rename + rewire)、tests/project_dir_resolve_test.rs(#10 去 helper 测试)。
- ccteam-web:auth.rs(#12 doc)。
