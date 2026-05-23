# ccteam V0.6.5 — V0.6 收尾:MCP 桥补全 + advise 落地 + UX cohesion + F113 验收补做

> **状态:doc-first / Wave 0**(本 PR 范围 = `README.md` + `prd.md` + `dev-plan.md`,代码 PR 走后续 Wave 1-3)。
> **主题:** **关 V0.6 的账**。V0.6.0 立项时承诺的 chat-mode end-to-end onboarding(从 `/ccteam-creator` 跑下来到 TG 收回复)从未真正通过 ── 长期被「老用户手工补 registration 文件」掩盖,2026-05-23 nas-box005 的 Telegram duplicate flood 调研把 chain of root causes 全暴露出来。本版把 V0.6.0 Wave 2/3 应交未交的 MCP `chat_*` / `advise_*` 表面 + creator 注册桥 + advise vote + UX 决策树 + F113 验收 #5 (50-query intent test)一次合上。
> **基线起点:** workspace `0.6.4` / test `1482 / 1`(V0.6.4 ship 数 + 那次 OutboundCursor PR 的 7 新测试)/ clippy `-D warnings` clean。
> **基线目标:** workspace `0.6.5` / test ≥ `1530 / 1`(+48 估算,见各 Epic 验收)/ clippy `-D warnings` clean。
> **痛点对应:** 13 用户痛点中 ── 痛点 4 (零摩擦上手)、痛点 5 (能否真正在 IM 闭环)、痛点 6 (跨 vendor 第二意见)、痛点 11 (Skill / 入口可发现性)。

---

## 0. 概览

| Epic | Finding | 一句话 | 体量 |
|---|---|---|---|
| **E — MCP 桥 + creator 通路**(P0)| **F146** | `mcp__ccteam__chat_register_bot` + `chat_list_bots` 真实现(拆 lifecycle 为原子操作)| M |
|  | **F147** | `mcp__ccteam__chat_send_input` + `chat_history` + `chat_reset` 真实现 | M |
|  | **F148** | `/ccteam-creator` Phase 5.6 调 F146,end-to-end 跑通(fresh machine → 收 TG 回复 0 手工)| M |
|  | **F149** | `/ccteam` 总入口移除 Wave 2/3 fallback 提示;intent 2/3 路径合上 | S |
|  | **F150** | `/ccteam-control` 接 `admin_*` MCP(冒烟验证 + 文档补)| S |
|  | **F151** | unregister 路径打通(`ccteam remove --purge` 也清 `imd/registry/`)| S |
| **F — Advise + Codex critic**(P1)| **F152** | `mcp__ccteam__advise_vote` 真实现(基于已有 `CodexExecAdapter` + claude-tui)| M |
|  | **F153** | `mcp__ccteam__advise_parallel` 真实现 | S |
|  | **F154** | `/ccteam-advise` skill body 移除 "Wave 3 落地" 占位语,改成真路径文档 | S |
|  | **F155** | `ccteam-creator` Phase 3.5 Codex auto-critic 验证 + 必要 gate | S |
|  | **F156** | `/ccteam-team` §3.5 N≥3 critic auto-injection 验证或显式 V0.7 推迟 | S |
| **G — UX cohesion + F113 收账**(P0/P1 混)| **F157** | `ccteam-scan --quick` 60s 内出小报告 + `/ccteam` 增加 "code-scan" 入口 | M |
|  | **F158** | `docs/task-to-command.md` 决策树 + README/quickstart/user-manual lead 改写 | S |
|  | **F159** | `/ccteam` 对未实现 intent 直接隐藏(不再 placeholder)| S |
|  | **F160** | CLAUDE.md §一 baseline 更新 + skill 状态注释清理 | S |
|  | **F161** | `/ccteam` dispatcher 文案 drift sweep(6 处 stale Wave 1 fallback)| XS |
|  | **F162** | F113 验收 #5 补做:50-query intent classifier accuracy test ≥90% | M |

**总计** 17 finding · 3 Epic · 估算 9-12 工作日(单线作业)或 4-5 天(并行 worktree)。

---

## 1. 为什么 V0.6.5 而不是 V0.7.0

V0.6.0 立项 PRD 里 F108 / F112 / F113 / F114 / F117 都明确承诺过 Wave 2/3 落地。**Wave 2/3 实际 ship 了 imd 传输层 + tmux supervisor + 路由器,但 MCP `chat_*` / `advise_*` 工具表面留在 Wave 1 STUB 状态 ── 这是 V0.6.0 自身没收的账,不是新功能**。同理:

- F113 验收 #5 "50-query intent classification ≥90%" 在 v0-6-0 PRD line 61 是 ship gate,Wave 4 doc-syncer 时没补、V0.6.1-V0.6.3 三个 patch 也都没补 ── 这是 V0.6.0 的欠债。
- `/ccteam-creator` Phase 5.6 说 "call `ccteam_imd::register_bot(...)`" ── Rust 函数存在但**没人能从 Claude TUI 外部调到它**(没 MCP / 没 CLI 子命令)。这条桥从 V0.6.0 立项以来就是断的。

把这些归到 V0.7.0 等于把"V0.6 闭环"主题往后推一个 minor。V0.6.5 单独 ship 这 17 条,让 V0.6 真正闭环,V0.7.0 才能干净地开新主题(国内 IM / monorepo `.mcp.json` / chat memory sync / 第 4 mode HumanApproval 深化)。

**与 CLAUDE.md §五 "pre-v1.0 不留技术债" 原则一致** ── 不写 backward-compat shim、不留 deprecated alias;workflow.yaml / registry schema 直接前进。

---

## 2. 痛点映射(13 用户痛点)

| 痛点 | 本版相关 finding | 解释 |
|---|---|---|
| 4 (零摩擦上手) | F157 / F158 / F159 | `ccteam-scan --quick` 是新用户 60 秒看到 value 的零依赖路径;决策树替代 mode/preset/recipe 三层认知 |
| 5 (IM 闭环) | F146 / F147 / F148 / F151 | chat MCP 桥补全后,**从未跑通**的 `/ccteam-creator → daemon → TG` end-to-end 第一次真正可重现 |
| 6 (跨 vendor 第二意见) | F152 / F153 / F154 / F155 | advise vote / parallel MCP 真实现,Codex auto-critic 验证 |
| 11 (Skill / 入口可发现性) | F149 / F158 / F160 / F161 | 用户面文档 + dispatcher 文案对齐当前真实状态;F113 验收 #5 补做让"分类准"有数字 |

---

## 3. 红线核对(CLAUDE.md §三)

| 红线 | 本版触及? | 守的方式 |
|---|---|---|
| 文件系统是控制平面 | F146 / F147 / F151(写 / 删 `~/.ccteam/imd/registry/`) | 仍只走 ccteam-owned dir 下文件;MCP 工具内调 `register_bot()` / `unregister_bot()`,行为 = Rust 函数;不引入新 IPC |
| `progress.jsonl` 是唯一 state SoT | F146-F151 不写 progress | bot lifecycle 事件由 BotSupervisor 自己写 progress,MCP 工具不直接写 |
| No prompt injection | F147 `chat_send_input` 写 mailbox 文件 | mailbox 是已有路径,与 inbox 同级别处理;不向 tmux pane 直接 send-keys system prompt |
| 每次 spawn = fresh 1M context(bg 模式) | 不触及 | chat 模式 spawn 复用 context 是 feature(CLAUDE.md §三 原文) |
| 永不主动 kill 长 session | F146-F151 不 kill | `chat_lifecycle.stop` 走 graceful `close_thread`;`chat_reset` 同 |
| 不解析 tmux 终端输出 | 不触及 | F147 走 mailbox 写文件,不 scrape |
| fix-loop 撞 3 次必 escalate | 不触及 | 本版无 fix-loop 改 |
| `ccteam-core` 零 team 名字面量 | 不触及 | 本版 ccteam-core 改动仅在 cli / imd / mcp 层 |
| 跨项目记忆走官方接口 | 不触及 | 本版不改 `~/.claude/CLAUDE.md` / `~/.codex/AGENTS.md` 处理 |
| 新建项目走 `<projects_root>/<team>-<slug>/` | F151 涉及 remove | 沿用 `pick_unused_slug` 强制 team 前缀;remove --purge 一并清 imd registry |
| root README.md MUST be English | F158 改 README | F158 决策树插入位置走英文段(中文版进 `docs/quickstart.md` / `docs/user-manual.md`)|
| README.md 不含版本进展/状态信息 | F158 改 README | 决策树是"产品当前能力"展示,**不**含版本号 / shipping 日期 / 验收数字 |
| HITL approval state SoT(V0.6.1 F124) | 不触及 | 本版无 plan_approval 改 |

---

## 4. 不在范围(V0.7 候选)

| 项 | 推到 |
|---|---|
| Slack / Discord onboarding(provider Rust 已就位,`ccteam-im-setup` skill 拒绝)| V0.7(已在 V0.6.0 PRD §F117 锁定为 V0.7 范围)|
| Lark / DingTalk / WeChat / QQ Channel | V0.7 Epic C |
| User-defined personas(用户写 markdown)| V0.7(`ccteam-creator-persona-new` flow)|
| Voice / 图片 / 多模态 input | V0.7+ |
| monorepo-aware `.mcp.json` | V0.7+(researcher R6#4)|
| `ccteam migrate-from-claude` 反向 import | V0.7+(codex-expert CX6#5)|
| DM 跨设备 sync(chat memory)| V0.7+ |
| non-Anthropic/non-OpenAI vendor(Gemini / DeepSeek / Qwen)| V0.7+ |
| ccteam-core 巨石拆分(audit team REQ-006)| V0.7.x patch |
| ProjectState 上帝对象(audit REQ-002)| V0.7.x patch |
| 文件即 IPC → Event Bus(audit REQ-004)| V0.7.x or 独立 minor |
| 老 daemon 不响应 SIGINT/SIGTERM(2026-05-23 调研发现)| 独立 finding,V0.6.6 或 V0.7 |
| claude-tui adapter 重附接已存在 tmux session(同上)| 独立 finding,V0.7 |

---

## 5. Ship gate(V0.6.5 → main)

1. **baseline**:`cargo test --workspace --locked --no-fail-fast` ≥ **1530 / 1**(1482 起点 + 各 finding 验收测试)
2. **clippy**:`cargo clippy --workspace --all-targets --locked -- -D warnings` 0 命中
3. **`/ccteam-creator` end-to-end**:fresh machine + 零手工写文件 → `/ccteam-creator "做个 TG 助理"` → `go` → daemon 起 → TG 双向通(测试人确认)
4. **F113 验收 #5 数字**:`scripts/host-probe/intent-accuracy.sh` 输出 ≥ 90% accuracy + confusion matrix 落 `docs/versions/v0-6-5/intent-accuracy.md`
5. **tier-1 docs 文案 grep**:
   - `grep -rn "Wave [123]\|wave2-not-ready\|Wave 3.*未落地" skills/*/SKILL.md` 全 0 命中
   - `grep -rn "mode:.*chat\|preset:.*chat-pocket" docs/{quickstart,user-manual}.md README.md` 全 0 命中(决策树取代,内部架构词汇下沉到 docs/advanced/)
6. **CLAUDE.md §一 baseline 表更新到 V0.6.5 数字** + §四 skill 状态注释清理
7. **`cargo run --release -- doctor`** 输出含 "MCP tool surface: 26 active, 0 stubs"(原 9 个 stub 全实现)

---

## 6. Wave 结构

详 `dev-plan.md`。概要:

```
Wave 0  doc-first(本 PR)
        ├── README.md           ← 本文件
        ├── prd.md              ← 17 finding 完整需求
        └── dev-plan.md         ← worktree-per-epic + acceptance gate

Wave 1  Epic E(MCP chat 桥 + creator)── P0 解今晚 flood 真因
        worktree-per-finding F146-F151,串行依赖 F146 → F148

Wave 2  Epic F(advise + Codex critic)── 并行
        worktree-per-finding F152-F156,F152/F153 可并行

Wave 3  Epic G(UX cohesion + F113 验收)
        F157-F160 并行;F161 文案 sweep;F162 intent fixture + 跑 + 报告

Wave 4  doc-syncer + host-probe + ship gate
        CLAUDE.md baseline 回填 + tier-1 docs 同步 + version bump 0.6.4 → 0.6.5
```

每 Wave PR 必须 baseline ≥ 上 Wave 数字 + clippy 0 警告,否则不发。

---

## 7. Doc-first 完成判据(本 PR 验收)

- [ ] `README.md`(本文件)落
- [ ] `prd.md` 17 finding 各章节完整(痛点 / 现状缺口 / 设计 / 文件 / 验收 / 风险)
- [ ] `dev-plan.md` 含 4 Wave + worktree 分配 + acceptance gate
- [ ] CLAUDE.md `§一 当前状态` 表 `当前最新版` 行**暂不动**(代码 ship 后回填)
- [ ] 用户 review pass → merge → Wave 1 kick-off
