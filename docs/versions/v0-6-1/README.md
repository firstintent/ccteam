# V0.6.1 — End-to-end user-usable + 全文档清晰 + 遗留全完成

> **Status**:**SHIPPED 2026-05-19**(`git tag v0.6.1`,commit `95916d3`)。
> Baseline 1365/1 · clippy `-D warnings` clean · 26 MCP tools · 12 finding · 3-wave 压缩范式 · ship-gate via E2E 8-path sim PASS。
>
> **立项决策(`2026-05-19` 用户拍板)**:
> - **不留遗留** — V0.6 retained 6 finding + V0.5 deferred F98 + 3 doc-quality 新 finding + 2 user-manual-claim 实现 finding = **12 finding 全 ship V0.6.1**(版本号保 patch,scope 接近 minor)
> - **EN-only root README** — CLAUDE.md 加红线 "root README.md MUST be English",删现 CN README,落 EN 版
> - **user-manual.md 100% 亲测可用** — 凡 user-manual 写的 slash / action / claim,V0.6.1 ship 时**全部 live-tested 通过**;漂移项不允许"标 V0.7"或"删了之",**先实现再 ship**
>
> **一句 value prop**:V0.6.0 "shipped product-ready" → V0.6.1 "shipped *honest* product-ready"。
> 用户读的每行文档都对应一行能跑的代码;每个宣称的 slash 都真存在;EN OSS 入口干净。
>
> **不开新 mode**:F124(6 号编排模式 HITL / Approval Gating)虽是 V0.6 PRD §八 延期项,本版按"遗留全完成"硬约束 ship;但 narrow scope = plan-approval 流走 outbox 走通即可,不开"V0.7 大功能"门。

---

## 一、Epic 主线(3 Epic,12 finding)

### Epic D — Cleanup & honesty(本版主题,V0.6.0 ship 后的"债务回购")

**目标**:把 V0.6.0 ship 时挂账的 risks / deferred / known-drift 一次性还清,让"读到 = 跑得通"。

下属 finding:F119 / F120 / F121 / F122 / F123 / F125 / F126 / F127

### Epic E — User-claim 实现(V0.6.0 user-manual 写了但未实现的功能)

**目标**:V0.6.0 user-manual.md 描述但未实现的用户面 slash + IM NL admin 全部补齐,达"读到 = 用得上"。

下属 finding:F128(`/ccteam-control` 扩展)/ F129(`@ccteam` IM NL admin)

### Epic F — Plan/Approval 落地(V0.5 + V0.6 retained,本版闭环)

**目标**:plan-first 工作流真走通(plan 写出 → outbox 推 → HITL approve → resume),给"长跑任务夜里改方向"落地手段。

下属 finding:F98(plan-approval ↔ outbox 联动)/ F124(HITL Approval Gating,narrow scope)

---

## 二、Findings 索引(F98 + F119-F129)

| F | 主题 | 性质 | Epic | Wave |
|---|---|---|---|---|
| F98 | plan-approval ↔ outbox 联动(plan 写完推 IM → user 回 `approve` resume)| 新增 | F | 2 |
| F119 | probe script 起 ccteam-imd daemon + health-wait + mode-3 real round-trip | 工具增强 | D | 1 |
| F120 | overnight-builder probe full workflow(fake artifact + assert agent_done)| 工具增强 | D | 1 |
| F121 | `ccteam doctor --check-pricing-version` 警告(pricing/*.toml schema_version >180 天 提醒)| 增强 | D | 1 |
| F122 | CodexAppServerAdapter notifications → progress.jsonl bridge | bug fix | D | 1 |
| F123 | 5 demo GIF 录制(asciinema → agg,Solo / Team / Overnight / Pocket / Squad)| 文档 | D | 3 |
| F124 | HITL Approval Gating(narrow scope:plan-approval 流支持 IM `approve`/`reject` 控制 workflow continue)| 新增 | F | 2 |
| F125 | 全局文档审计 + cross-doc 一致性 sweep + 历史 v0-X-Y 归档清理 | 文档大扫除 | D | 1 + 3 |
| F126 | README.md EN-only rewrite + CLAUDE.md 加红线 "root README.md MUST be English" | 文档 | D | 1 |
| F127 | user-manual.md 100% 亲测可用 sweep(每行 slash/action live-test)| 文档 + 验证 | D | 3 |
| F128 | `/ccteam-control` 扩展 change-persona + add-tool subcommand(MCP `admin_change_persona` + `admin_add_tool`)| 新增 | E | 2 |
| F129 | `@ccteam` IM NL admin via meta-agent(`pause`/`resume`/`list bots`/`cost today`/`stop everything`)| 新增 | E | 2 |

12 finding,跨 3 Epic;F125 跨 wave 跑(Wave 1 落 sweep / Wave 3 finalize)。

---

## 三、3 Wave 流程(patch 4-wave 范式压缩版)

V0.6.0 4-wave 范式;V0.6.1 patch 压成 **3 wave**:

### Wave 0 — doc-first(本 session)

- 本文件 + `prd.md` + `dev-plan.md` 落 `docs/versions/v0-6-1/`
- 用户 review → "go" 才 TeamCreate

### Wave 1 — Cleanup & bridges(4 teammate 并行,~1h)

| Teammate | Findings | Worktree branch |
|---|---|---|
| probe-fix | F119 + F120 | `v061-w1-probe-fix` |
| cost-doctor | F121 | `v061-w1-cost-doctor` |
| codex-bridge | F122 | `v061-w1-codex-bridge` |
| doc-curator | F125(sweep)+ F126(EN README + CLAUDE.md 红线)| `v061-w1-doc-curator` |

**Acceptance gate per Wave 1 PR**:
- baseline ≥ 1283/1(`env -u HTTP_PROXY ... cargo test --workspace --locked --no-fail-fast`)
- clippy `-D warnings` clean
- 红线 grep(per F125 spec)清零
- 主仓 main 不动 dirty(独立 worktree)

### Wave 2 — User-claim 实现 + Plan/Approval(4 teammate 并行,~2h)

| Teammate | Findings | Worktree branch |
|---|---|---|
| plan-approval | F98 | `v061-w2-plan-approval` |
| hitl | F124(narrow scope)| `v061-w2-hitl` |
| control-ext | F128(`/ccteam-control` change-persona + add-tool)| `v061-w2-control-ext` |
| im-nl-admin | F129(`@ccteam` meta-agent)| `v061-w2-im-nl-admin` |

**Acceptance gate**:同 Wave 1。

### Wave 3 — Verification + ship(3 teammate 并行 + 主 session 整合,~1.5h)

| Teammate | Findings | Worktree branch |
|---|---|---|
| manual-prover | F127(逐行 live-test user-manual.md)| `v061-w3-manual-prover` |
| demo-recorder | F123(5 GIF asciinema → agg)| `v061-w3-demo-recorder` |
| doc-syncer | F125 finalize(tier-1 doc 同步 + CLAUDE.md baseline 回填 + dev-coupling-audit 加 F98 + F119-F129)| `v061-w3-doc-syncer` |

主 session 整合:
- 3 PR 顺序 review/merge(plan-approval / hitl / control-ext / im-nl-admin 已 land main)
- `workspace.package.version` bump 0.6.0 → 0.6.1
- nas-box005 host probe 全 12 finding(F119+F120 enhanced script 跑 5 preset + 3 codex + 1 plan-approval + 1 hitl)
- `git tag v0.6.1 && git push origin v0.6.1`
- TG `@web3op_bot` ping ship done

---

## 四、必读上下文(下 session 接力时)

按顺序:

1. 本文件(Epic 主线 + 12 finding + 3 wave 流程)
2. `prd.md`(每个 finding 详细 PRD)
3. `dev-plan.md`(wave-by-wave teammate briefing + acceptance gate)
4. `CLAUDE.md`(项目红线 + Pre-v1.0 不留技术债 + 4-wave 范式)
5. `docs/versions/v0-6-0/wave-{1,2,3,4}-handoff.md`(V0.6 实施 + retained risk 原始记录)
6. `docs/versions/v0-6-0/host-probe.md`(V0.6 probe gap 详)

---

## 五、决策记录

| 决策 | 选项 | 选了 | 理由 |
|---|---|---|---|
| F124 scope | V0.6.1 全开 vs 推 V0.7 | **V0.6.1 全开(narrow scope)** | 用户硬约束"遗留全完成";F124 限 plan-approval 流支持 IM approve/reject,不开 6 号 mode 大门 |
| README.md 英语化 | EN-only 删 CN vs EN+zh/ 双语 | **EN-only** | OSS 主入口干净;docs/quickstart/user-manual 继续 CN(国内用户面)|
| user-manual 漂移项 | prove-or-trim vs build-everything | **build-everything**(F128 + F129)| 用户硬约束"读到 = 用得上";V0.6.1 patch 体积变大,可接受 |
| 立 V0.7 minor 还是保 V0.6.1 patch | 12 finding 体量已接近 minor | **保 V0.6.1 patch** | 用户明确用 V0.6.1 命名;feature-heavy patch ≠ minor(无 epic 重写)|
| release 形式 | tag only vs github release page | **tag only**(同 V0.6.0)| 用户 V0.6 已定 |
| ping 政策 | milestone-only vs verbose | **milestone-only**(wave merge ✓ / final ship ✓ / 真 blocker)| V0.6 沿用 |

---

## 六、不在本版(V0.7 / 更晚)

- **国内 IM 启用**(WeChat / 飞书 / DingTalk / QQ)— V0.7 主线
- **chat memory 跨设备同步** — V0.7
- **monorepo-aware `.mcp.json`** — V0.7+
- **`ccteam migrate-from claude` 反向 import** — V0.7+
- **6 号编排模式深化**(F124 V0.6.1 仅 narrow scope plan-approval 流;true HITL workflow 大改 V0.7+)

---

详 `prd.md` + `dev-plan.md`。

---

## 七、Ship-day fix-in-version(2026-05-19 user directive)

V0.6.1 tag 已落(`v0.6.1` / commit `95916d3`),但 ship-day 用户上手后发现 3 个 daemon / probe / IM 体验问题,本版**就地修复**(版本号不动,patch in-place):

| F | 主题 | 性质 | Wave / 单线 |
|---|---|---|---|
| F130 | `ccteam-imd` 折入 `ccteam start`(单进程:orchestrator + web + IMD supervisor 3 个 tokio 任务;独立 `ccteam-imd` 二进制删;`--no-imd` 跳过)| 进程模型简化 | ship-day fix |
| F131 | host-probe `remote_run` 单引号包裹 bug 修(`bash -c '<heredoc>'` 嵌套引号 escape)| bug fix | ship-day fix |
| F132 | IMD inbound wire — `run_daemon_with_shutdown` 现在 spawn `Channel::listen` task + mpsc 消费者 + 每 tick `drain_inboxes`(`<projects>/<slug>/.ccteam/chat/<role>/inbox/*.md` → `BotSupervisor::handle_inbound` → `submit_turn`)| critical user-facing bug fix | ship-day fix |

**决策**:
- F130 = 用户上手时遇到僵尸进程排查痛点 → 单进程 daemon 让管理优雅(对照 V0.4.x web fold-into-start 范式)。Pre-v1.0 no-shim:`[[bin]] ccteam-imd` 直接删,无 alias,无 `ccteam daemon` 子命令(folded into `ccteam start` 即足)。
- F131 = probe 实跑暴露的 shell quoting 转义错(详 wave-3 ship-day log)。
- F132 = NAS user-chat test 发现 `web3op_bot` 收到 TG message 但完全无反应。Root cause:`TelegramChannel::listen()`(getUpdates long-poll)+ `process_inbound_admin_aware`(mailbox writer)+ `BotSupervisor::handle_inbound`(`submit_turn` → tmux pane)三段代码都存在,但 `run_daemon_with_shutdown` 旧版只 tick supervisors,**从未 spawn channel listener,也从未读 inbox 目录**。F132 daemon 现在:(1) 启动时按 `creds.telegram` + 注册 bot 的 `im_chat_id` allowlist 构造 `TelegramChannel`,spawn `listen` task,(2) 单一 inbound consumer task 把 mpsc 中的 `ChannelMessage` 走 `process_inbound_admin_aware`,(3) 每 supervisor tick 后立刻 `drain_inboxes` — 排序 inbox `.md` 文件、调用 `handle_inbound`、删除文件(one-shot)。Test injection 通过新 `DaemonArgs::channels_override` field(MockChannel 注入)。新 integ test `crates/ccteam-imd/tests/inbound_wiring_test.rs::daemon_wires_mock_channel_to_supervisor_inbox` 端到端 assert 一条 MockChannel `@lead` 消息 → mailbox file 出现 → 删除 → stub adapter `submit_turn` 计数 += 1 + payload 是 stripped(`"please look at this"`)。**Outbound (turns.jsonl → sendMessage) wire-up 不在 F132 scope** — 函数 `outbound::forward_new_rows` 已实现但同样未被 daemon 调用;留给 follow-up(NAS 用户已可见 bot tmux pane 接收到自己消息这一半 round-trip)。

