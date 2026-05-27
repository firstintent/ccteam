# Session Handoff — rmux integration (v0-8-rmux-integration)

> Paste-ready prompts for migrating to a new session. Branch is at 69
> commits, all pushed to `origin/v0-8-rmux-integration`, baseline green,
> worktree `/tmp/ccteam-rmux` (target/ warm after last rebuild).

---

## ① GOAL prompt — recommended (achievable in-environment)

Re-issue this with `/goal` in the new session. The agent CAN complete it
(unlike the original, which implies macOS/Windows hardware the sandbox
lacks and would block the stop-hook forever):

```
继续在 v0-8-rmux-integration 分支(worktree /tmp/ccteam-rmux,不发版不PR,≤3 opus subagent)推进 rmux 集成收尾。完成可在本环境验证的剩余项:(1) flip-default —— 按 docs/versions/v0-8-rmux/w-flip-default-migration-plan.md Steps 1-5,把 adapter 级测试 pin 到 CCTEAM_MUX_BACKEND=tmux,翻转 default_backend() 默认到 rmux,全 workspace 测试绿(--exclude ccteam-web;daemon_dm_* / daemon_wires_mock_* 是 inotify flake 可忽略);(2) EnrichedEvent merger 接入 subscribe 或显式标注为 ahead-of-consumer 基础设施。每步 commit + clippy 0 + fmt clean + push origin。macOS/Windows CI 绿与 merge-to-main burn-in 属硬件/时间外因,不纳入完成判定。直到上述两项达成。
```

## ①′ GOAL prompt — original (verbatim)

⚠️ As literally written this can't be satisfied in a Linux sandbox
(macOS/Windows production validation needs hardware) — the stop-hook
will block indefinitely. Use only if you accept that.

```
新开一个rmux分支，分支不用发版，不用pr，在分支上连续开发即可，最终目标是整个rmux集成完成到100%，使用opus subagent。最大subagent不超过3个。直到所有功能完全就绪。达到用户生产级别。
```

---

## ② RESUME prompt — work context (paste after, or let the agent read this file)

```
继续 rmux 集成(分支 v0-8-rmux-integration,worktree /tmp/ccteam-rmux,不发版/不PR,持续开发,≤3 opus subagent)。

起手:cd /tmp/ccteam-rmux && cargo build --workspace --locked(target 已暖,增量编译)。
git log --oneline -5 看 HEAD(69 commits 全推送 origin)。
验证 gate:cargo clippy --workspace --all-targets --locked -- -D warnings(须 0)、
cargo fmt --all -- --check(须 clean)、
cargo test --workspace --locked --no-fail-fast --exclude ccteam-web
(注:daemon_dm_no_at_mention_auto_routes_to_single_bot + daemon_wires_mock_channel_to_supervisor_inbox
是本机 inotify 耗尽 flake,非回归,CI/单跑必过;基线 ~1672 pass)。

【已完成 W0-W6 全部】MuxBackend trait + Tmux/Rmux/InProc 三 backend;单 binary daemon(ccteam --__internal-daemon
re-exec + RMUX_SDK_DAEMON_BINARY=current_exe);全 mode(3a claude_tui / 2 claude_bg+codex_exec / 3b codex_app_server)
走 trait;subscribe + pattern registry(claude 10/codex 4) + EnrichedEvent merger(merger built-not-wired);
attach/peek/screenshot/web-SSE 全 from_env;exit-empty=off + dead-handle reconnect;6 个 Codex wire 缺陷修复;
W6 hook-reroute flag-gated(CCTEAM_HOOK_VIA_DAEMON 默认关)。端到端活体验证过(roundtrip + reconnect-after-death,
CI rmux-smoke job linux+macos matrix)。

【关键设计】rmux 是 crates.io "0.3" 依赖(可跟随升级);默认 backend 仍 tmux,rmux 走 CCTEAM_MUX_BACKEND=rmux opt-in;
default_backend()/from_env() honor 该 env(969b0e2)。红线:业务零 grep pane bytes;progress.jsonl 是 SoT。
文档全在 docs/versions/v0-8-rmux/ —— 先读 as-built-architecture.md + w-production-readiness.md
+ w-flip-default-migration-plan.md。

【剩余项】
① flip-default 全局切 rmux:实测 naive flip 让 7/8 adapter 测试 timeout(路由到未运行的 daemon)。
   按 w-flip-default-migration-plan.md Steps 1-5:inventory → pin adapter 测试到 CCTEAM_MUX_BACKEND=tmux →
   加 rmux-default 覆盖 → 翻 default_backend() 默认 → 全绿。commit-per-step、baseline-safe。
   受影响测试文件:claude_tui_resume_test / claude_tui_reattach_test / claude_tui_env_test / harness_trait_test
   + orchestrator 相关 + 任何经 default_backend() spawn 的。本分支(不发版)可做;merge-to-main 仍需 burn-in。
② macOS/Windows CI 绿:需 Darwin/Windows runner(CI 已接线,push 跑),本地无法验证 —— 外因,不纳入完成判定。
③ EnrichedEvent merger 接 subscribe + orchestrator 消费 PatternMatched:目前 built-not-wired,
   无真实 consumer(daemon-side rate-limit 自愈是其用例,未设计)。要么接最小 consumer,要么显式标注 ahead-of-consumer。

【纪律】subagent 只改自己 explicit-path 文件、绝不 git stash/checkout/add -A(共享 worktree);
每批后 clippy 0 + fmt clean + push origin v0-8-rmux-integration:v0-8-rmux-integration。
flip-default 的 test 迁移不要在上下文将满时启动(不能留半成品破基线)。
```

---

## Current state snapshot

| 项 | 值 |
|---|---|
| 分支 | `v0-8-rmux-integration`(off origin/main `446e33a`)|
| commits | 69,全部 pushed origin |
| 基线 | ~1672 pass(2 个 inotify flake 非回归)· clippy 0 · fmt clean |
| worktree | `/tmp/ccteam-rmux`(target 暖)|
| 默认 backend | tmux(rmux 走 `CCTEAM_MUX_BACKEND=rmux` opt-in,生产级 + 端到端验证)|
| 剩余 | flip-default 迁移(可做)· macOS/Windows CI(硬件)· EnrichedEvent consumer(基础设施)|

## 注意:`/goal` 是 session-scoped
迁到新 session **不会自动带 hook**。要继续 goal-driven 开发,在新 session 重新 `/goal`(用 ① 推荐版)。
不重发 = 普通会话,可正常收口。
