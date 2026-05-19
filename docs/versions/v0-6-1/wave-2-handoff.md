# V0.6.1 Wave 2 — Handoff

> **Status**: Shipped 2026-05-19 via 4 squash-merge to main(`81a725e` F124 → `3e4c283` F98 → `adbd96d` F129 → `085f5ee` F128)。
> **Baseline**: **1365 / 1**(Wave 1 整合后 1296/1 + 69 net new tests:F124 +6 / F98 +16 + 大量 unit / F129 +?? / F128 +?? + 内部 reshape)。1 fail = pre-existing SSE flake。
> **Clippy**: 0 errors,0 warnings(`-D warnings` clean)。
> **Wall time**: 4 teammate 并行 worktree ~30 min;主 session 整合 ~15 min。

## Decided

### F98 plan-approval ↔ outbox engine(PR #78)

- 新 `crates/ccteam-core/src/plan_approval.rs`(710 行)— pure state machine,跟 orchestrator 完全解耦(F124 hitl can stack 0 conflict)
- workflow.yaml `plan_approval:` block + `PlanApprovalSpec`(timeout_min / on_timeout 三选:`escalate` / `auto-approve` / `reject` / disabled when `enabled: false`)
- `plan_decision_required` + `plan_decision` 两 progress event(`progress.rs` +78 行)
- decision parser:`APPROVE` / `REJECT [<reason>]` / `EDIT <comment>`(IM 端用户回复语法)
- 9 new tests(`crates/ccteam-core/tests/plan_approval_test.rs` 434 行):schema round-trip / parser / APPROVE happy path with progress.jsonl ordering / REJECT-with-reason / 3 timeout modes / unknown-plan no-op / idempotent re-decide
- 闭 V0.5 deferred F98

### F124 mode: human-approval narrow scope(PR #77)

- `WorkflowMode::HumanApproval` 第 4 mode(enum variant in workflow.rs + serde)
- orchestrator pick_adapter + dispatch gate — artifact event parks on pending + emits `plan_decision_required`;`poll_completions` skips drain for paused agents
- CLI `commands.rs` +11 行(workflow mode display string)
- CLAUDE.md §三 红线 row:`| **HITL approval state SoT** | — | progress.jsonl::plan_decision | 同 |`
- 6 new tests(3 workflow round-trip + 3 orchestrator dispatch)
- **协作设计**:F124 owns mode enum + dispatch gate;F98 owns IM round-trip + plan_decision injection;两者共用 progress.rs event types,workflow.yaml schema 无 overlap

### F128 /ccteam-control change-persona + add-tool(PR #80)

- 新 `crates/ccteam-core/src/admin_actions.rs`(396 行)— pure file-mutation engine(读 `<project>/.claude/agents/<bot>.md` + merge / 读 workflow.yaml + tools append)
- 新 `crates/ccteam-cli/src/mcp_admin_tools.rs`(210 行)— 2 MCP 工具 `admin_change_persona` + `admin_add_tool` schema + dispatcher
- `mcp_serve.rs` + `mcp_tool_groups.rs` 接入 admin group(原 V0.6.0 5 group 内的 admin group +2 tool;总数 24 → 26)
- `skills/ccteam-control/SKILL.md` +41 行 — 2 子命令文档 + LLM 提示模式(NL → md merge)
- CLI `ccteam admin change-persona / add-tool` 走 MCP path
- 2 new tests(`admin_change_persona_test.rs` 122 行 + `admin_add_tool_test.rs` 173 行)
- 闭 user-manual.md §2.4 漂移(`/ccteam-control change-persona helper-bot "..."` + `add-tool` 现在真实现)

### F129 @ccteam IM NL admin via meta-agent(PR #79)

- `crates/ccteam-imd/src/nl_admin.rs`(+528 行)— 5 keyword admin action(pause / resume / list / cost / stop everything)+ 危险动作 2 步 confirm flow(`stop everything` 二次 CONFIRM)
- `crates/ccteam-imd/src/inbound.rs`(+58 行)— `@ccteam <NL>` mention 检测路径,在 `@<bot>` route 之前
- hop_limit 不消耗(meta-agent admin path 不算 bot-to-bot hop)
- 集成测试 `im_nl_admin_test.rs`(367 行)— mock TG inbound w/ 5 NL admin path + 危险 confirm flow
- 闭 user-manual.md §3.2 漂移(`@ccteam pause/resume/list bots/cost today/stop everything` 现在真识别)

## Rejected

- ~~F124 rebase on F98 before merge~~ — teammate report 验证 0 conflict surface(F124 owns mode enum + dispatch gate;F98 owns plan_approval engine + IM round-trip),merge order 无关。最终顺序:F124 (#77) → F98 (#78) → F129 (#79) → F128 (#80)。
- ~~hitl teammate 实现 IM round-trip~~ — F124 narrow scope = mode enum + dispatch only;IM 路径完全靠 F98 plan-approval。
- ~~control-ext + im-nl-admin 等 F128 / F129 完整后才挂 user-manual.md 改~~ — F127 manual-prover Wave 3 scope 内做(避免 W2 4-teammate 互相 sync wait)。

## Risks(待 Wave 3 兜)

- **F128 + F129 实现但 user-manual.md 还引用未实现的细节** — `/ccteam-doctor` 在 troubleshooting.md L1 仍写 `/ccteam-doctor` slash(实际是 CLI)。F127 manual-prover 必处理。
- **F124 narrow scope = mode enum + dispatch only** — 完整 HITL UX 流(IM message format / cost cap 联动 / TG vs Slack 适配)由 F98 plan-approval 涵盖,但实际 end-to-end 工作链路 Wave 3 host probe 必走一遍。
- **新增 30 MCP tool**(原 24 + F128 +2 + F129 +1 + F98 plan_approval 路径 +1 + 内部 housekeeping = ~28-30)— Wave 3 doc-syncer 在 `interfaces.md` 子前缀 sweep 重算确切数字 + update CLAUDE.md §四。
- **plan-approval engine 是 pure state machine 没 orchestrator 接入** — F98 PR teammate 选了"separate file" 设计避免 W2 内 conflict;Wave 3 manual-prover E2E sim 时 verify orchestrator main loop 真调 plan-approval engine。

## Files

新文件(7):
- `crates/ccteam-core/src/{plan_approval, admin_actions}.rs`(F98 + F128)
- `crates/ccteam-core/tests/{plan_approval_test, admin_change_persona_test, admin_add_tool_test}.rs`(F98 + F128 tests)
- `crates/ccteam-cli/src/mcp_admin_tools.rs`(F128 MCP)
- `crates/ccteam-imd/tests/im_nl_admin_test.rs`(F129 test)
- `docs/versions/v0-6-1/wave-2-handoff.md`(本文件)

修改:
- `crates/ccteam-core/src/{workflow,orchestrator,progress,lib}.rs`(F98 + F124)
- `crates/ccteam-cli/src/{commands,main,mcp_serve,mcp_tool_groups}.rs`(F124 + F128)
- `crates/ccteam-imd/src/{inbound,nl_admin}.rs`(F129)
- `skills/ccteam-control/SKILL.md`(F128 子命令文档)
- `CLAUDE.md`(F124 HITL 红线 row)
- 9 test files 调整 imports

中间整合 commit(W1.5):
- `7832fa0 docs: relocate per-version archives to docs/versions/v0-X-Y/`(用户 mid-W2 instruction;142 files,74 rename + 68 modify;global sed 加 versions/ 前缀)

## Remaining(Wave 3)

W3-T1 manual-prover F127(**扩展 scope**:用户加 ship policy "E2E sim 100% clean 才 tag v0.6.1;sim 发现 bug 在本版修不发新版")— 详 `dev-plan.md` Wave 3 manual-prover 段。

W3-T2 demo-recorder F123(5 GIF asciinema → agg)。

W3-T3 doc-syncer:
- F125 finalize:tier-1 docs(tech-design / interfaces / dev-coupling-audit / claude-code-tool-surface)同步 F98 + F119-F129;MCP 工具数 24 → ~28-30 重算
- CLAUDE.md §一 baseline 数字回填(本 wave 1365/1)+ workspace version → 0.6.1
- dev-coupling-audit 加 F98 + F119-F129 共 12 行索引
- docs/versions/v0-X-Y/ 索引(docs/README.md table)— V0.6.0 + V0.6.1 已加 by 主 session(本 wave 整合时)

主 session ship:
- W3 PRs review/merge
- workspace.package.version bump 0.6.0 → 0.6.1
- nas-box005 full host probe
- **等 manual-prover PR title 含 `[ship-gate-pass]` 才 tag**
- `git tag v0.6.1 && git push origin v0.6.1`
- TG ping ship done
