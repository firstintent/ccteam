# ccteam V0.6.6 — 低摩擦安装 + V0.6.5 后清扫 + mode-3 上下文恢复 + Codex 成本统一

> **状态:doc-first / Wave 0**(本 PR 范围 = `README.md` + `prd.md` + `dev-plan.md`,代码 PR 走后续 8 个 worktree-per-finding 并行 dispatch)。
> **主题:** **关 V0.6.5 ship 留的小账 + 把两条原 V0.7 候选拉回 V0.6.6**。V0.6.5 闭了 V0.6 自身的账(chat MCP 桥 / advise / UX cohesion / 运维 robustness),`post-ship-stub-inventory.md` 又给出一张 codebase 干净度地图。本版按那张地图扫尾,加 **F172 mode-3 上下文恢复**(daemon 重启不丢长跑 chat 会话上下文)+ **F173 daemon-routed Codex critic 统一 cost rollup**(V0.6.3 F156 留的 explicit defer 现在补完),同时把 **F166 prebuilt binary + install.sh** 这条「零摩擦安装」做掉 ── 用户不再被 `cargo install --git` 拦在门口。
> **基线起点:** workspace `0.6.5` / test `1583 / 1`(V0.6.5 ship 数,1 fail 是已知 `workflow_summary_reflects_agent_spawn_and_done_events` flake)/ clippy `-D warnings` clean。
> **基线目标:** workspace `0.6.6` / test ≥ `1660 / 1`(+~80,见 §0 估算)/ clippy `-D warnings` clean。
> **痛点对应(13 用户痛点):** 痛点 4(零摩擦上手)/ 痛点 9(团队不依赖人在场,24/7 daemon)/ 痛点 11(Skill / 入口可发现性)/ 痛点 14(长跑可靠性)。

---

## 0. 概览

| Finding | 一句话 | 对应痛点 | 体量 | 估算新测试 |
|---|---|---|---|---|
| **F166** | GH Releases 预编译 binary + `install.sh` 一键装 | 4 | M | +12 |
| **F167** | `/ccteam-creator` 默认 workflow.yaml + role md 按 project 类型出 sensible defaults(**轻量,不是完整 template library**) | 4 / 11 | S-M | +6 |
| **F168** | active TODO sweep ── codebase 内 9 个分布 site,逐条决断 fix / EOL-delete / V0.7 显式 defer-with-justification | 14 | M | +10 |
| **F169** | `nl_admin::cost_today` 接真 `ccteam_cost` ledger(替 V0.6.1 占位 registry-count 返值) | 9 / 14 | S | +4 |
| **F170** | 陈旧 doc-comment scrub(post-ship-stub-inventory Cat 7,实测 4 site) | 14 | XS | 0 |
| **F171** | `ccteam doctor --verify-mcp` flag 加 stub-counter parity 自检 | 14 | S | +6 |
| **F172** | tmux mode-3 上下文恢复 ── progress.jsonl 加 `chat_snapshot` event 周期 dump,daemon 重启可重建 | 9 / 14 | L | +30 |
| **F173** | Codex daemon-routed critic 统一 cost rollup(F156 follow-through;原 V0.7 候选) | 6 / 14 | M | +15 |

**总计** 8 finding · 8 worktree 并行(完全独立)· 估算 +~80 新测试 → baseline 目标 `1660/1`。

---

## 1. 为什么 V0.6.6 而不是 V0.7.0

V0.6.5 ship 时 `post-ship-stub-inventory.md` §"Recommendations" 给了三档候选:V0.6.6 patch(3 项)/ V0.7 minor 主候选(5 项)/ V0.8+ 主线(4 项)。用户决策(本版 scope 拍板)是:

- **3 项 V0.6.6 patch 全收**:F168(TODO sweep,实际范围比 inventory 列的更广,9 site)/ F169(`cost_today` 接真 ledger)/ F170(Cat 7 doc-comment scrub)/ F171(doctor stub-counter parity)。
- **从「V0.7 minor 主候选」拉两项回 V0.6.6**:**F172 mode-3 上下文恢复** + **F173 Codex critic 统一 cost rollup**。理由:
  - F172 是「24/7 长跑可靠性」的最后一块拼图 ── V0.6.5 F163 给了 graceful SIGTERM、F164 给了 tmux reattach,但 **daemon 崩 / 重启时 chat bot 已积累的上下文会丢**(F164 reattach 物理 session,F118 `session_recovery` 重建 last-N turns,但「last-N turns 的语义损失」对长跑 bot 来说仍然不可忽略)。F172 让 progress.jsonl 周期 snapshot 关键 context,重启时 chat_handle re-attach 后用 snapshot 续上,把损失从「last-N turns」缩到「最后一次 snapshot 起的增量」。
  - F173 是 V0.6.3 F156 留的「explicit defer past V0.6.5」 ── V0.6.5 advise/Codex critic 路径全 ship 了,F156 提到的 unified cost rollup 现在前提齐备(`CodexExecAdapter` 已用;`<root>/cost-budget.json` ledger 已在 F152 引入)。本版补完后,Codex critic 调用统计与 main spawn 在同一账上,用户面 `ccteam doctor` + `@ccteam cost today`(F169)出真数,end-to-end 闭环。
- **加 F166 + F167**:F166 是用户级零摩擦入口(对齐痛点 4 ── 当前 README quickstart 要求 `cargo install --git`,新用户至少装 Rust toolchain 才能开始);F167 是用户首次 `/ccteam-creator` 时落地 yaml 的 sensible defaults,与 F166 一起把「first 5 min user experience」抬一个档。

**F167 明确边界**(防 subagent 滚雪球):F167 = **per-project-type 启发式 + 默认值微调**,不是 LLM-assisted 完整 role auto-gen,也不是 template library 扩张(那是 V0.7 epic,见 §4)。

**与 CLAUDE.md §五 "pre-v1.0 不留技术债" 一致** ── F168 决断里只有「fix / EOL-delete / V0.7 显式 defer-with-justification」三档,**禁止**「TODO ship in V0.6.7」式拖单。本版收账,不开新账。

---

## 2. 痛点映射(13 用户痛点)

| 痛点 | 本版相关 finding | 解释 |
|---|---|---|
| 4(零摩擦上手) | F166 / F167 | prebuilt binary + install.sh:`curl ... \| sh` 一行装,不依赖 Rust toolchain;F167:首次 `/ccteam-creator` 生成的 workflow.yaml / role.md 按 project 类型(monorepo / single repo / docs-only / scripts-only)给 sensible defaults,降低用户「拿到模板还得手改」的认知成本 |
| 9(24/7 daemon) | F169 / F172 | F169:`@ccteam cost today` 出真 ledger 数(对齐 V0.6.5 ship gate item #9);F172:daemon 重启不丢 chat bot 上下文,长跑 7x24 真正能扛重启(不只是 tmux session 物理 reattach,语义层也续上) |
| 11(Skill / 入口可发现性) | F166 / F167 | install.sh 给非开发者用户(运维 / 业务方)一条「不学 Rust 就能装」的路;F167 让 `/ccteam-creator` 默认输出更接近用户实际意图,减少「装完不知怎么开始」的退出率 |
| 14(长跑可靠性) | F168 / F170 / F171 / F172 / F173 | F168:9 个 TODO 全部决断,不留「悄悄留 in-code」;F170:doc-comment 不再误导未来 contributor;F171:doctor 自检 MCP stub counter,catch 倒退;F172:上下文恢复 ── 见痛点 9;F173:Codex critic 与 main spawn 同账,长跑成本可观测可控 |
| 6(跨 vendor 第二意见) | F173 | F156 follow-through:Codex critic 路径走 `CodexExecAdapter`,cost 统一计入 advise budget ledger,用户面看到真实跨-vendor 花销 |

---

## 3. 红线核对(CLAUDE.md §三)

| 红线 | 本版触及? | 守的方式 |
|---|---|---|
| 文件系统是控制平面 | F172 写 `chat_snapshot` 到 progress.jsonl | progress.jsonl 是已存在的 SoT,**不**新建第二个文件;snapshot payload 内联(或指向 turns.jsonl 内 byte-offset),不引入新 IPC 通道 |
| `progress.jsonl` 是唯一 state SoT | F172 **守 + 扩展** | `chat_snapshot` 是 chat-mode event 家族的新成员(继 F108/F118 的 `chat_session_started` / `chat_turn_user_prompt` / `chat_turn_completed` / `chat_session_reset` / `chat_session_reset_with_recovery` / `chat_compact_done` / `chat_hop_escalate` 之后)── **不是新的第 9 类业务 event,而是扩 chat-mode 子家族**(CLAUDE.md §一 的 7 类业务 event + chat 子事件已是同一 SoT);daemon 重启从 progress.jsonl tail 读最近 `chat_snapshot` → 走 F118 既有 `session_recovery::build_recovery_prompt` 路径 |
| No prompt injection | F172 snapshot 仅作为 recovery context 注入新 tmux pane | recovery 时注入的是 user prompt 形式(对齐 F118 `chat_session_reset_with_recovery` 既有路径),不是 system prompt;`.claude/agents/<role>.md` 仍是 agent 行为唯一 SoT |
| 每次 spawn = fresh 1M context(bg 模式) | 不触及 | F172 仅作用于 mode-3 chat;chat 复用 context 是 feature(CLAUDE.md §三 原文) |
| 永不主动 kill 长 session | F172 / F173 守 | F172 snapshot 是**辅助 + 非破坏** ── 周期 dump 到 progress.jsonl,不触 tmux pane,不发 send-keys,不影响 bot 当前活动;只有 daemon 重启时(daemon process 已死) 才走 recovery 路径,不算 kill;F173 critic 通过 daemon route 仍是 fire-and-forget 子进程,与 main spawn 独立 |
| 不解析 tmux 终端输出 | F172 snapshot 源 = transcript jsonl + turns.jsonl(已存在的 ccteam-owned 路径) | snapshot 内容由 `turns_mirror`(F108 引入)与 progress.jsonl event 拼合而成,**不**调 `tmux capture-pane` |
| fix-loop 撞 3 次必 escalate | F168 决断须 explicit | F168 任何「continue iterating later」式拖单 → escalate-with-question,不写入 prd 默认 |
| `ccteam-core` 零 team 名字面量 | 不触及 | 本版 ccteam-core 改动仅在 progress event const(F172)+ chat snapshot helper 函数,**无** team 名 |
| 跨项目记忆走官方接口 | 不触及 | 本版不改 `~/.claude/CLAUDE.md` / `~/.codex/AGENTS.md` 处理 |
| 新建项目走 `<projects_root>/<team>-<slug>/` | 不触及 | F167 改默认值,不动 slug minting |
| root README.md MUST be English | F166 改 README quickstart 段 | install.sh 一行说明走英文段,中文说明进 `docs/quickstart.md` |
| README.md 不含版本进展/状态信息 | F166 改 README | install.sh / GH Release 描述是「产品当前能力」展示,**不**含版本号 / shipping 日期 / 验收数字 |
| HITL approval state SoT(V0.6.1 F124) | 不触及 | 本版无 plan_approval 改 |

**F172 红线设计补充说明**:本版**明确**选「扩 chat-mode 子事件家族」而非「新建第 9 类业务 event」── 决策理由:`chat_snapshot` 与 F118 `chat_session_reset_with_recovery` 是同一抽象层级的孪生事件(一个写、一个读 recovery 路径),把它们划到不同 category 会让 progress.jsonl 消费者(`ccteam-imd::daemon` recovery / `ccteam-web` dashboard / `ccteam-control` admin)的分发逻辑撕成两套,违反 SoT 原则的 spirit。详 prd §F172。

---

## 4. 不在范围(V0.7 候选)

| 项 | 推到 V0.7 的理由 |
|---|---|
| `/ccteam-creator` 完整 template library + role auto-gen | F167 只做 sensible defaults 微调(轻量);完整 template library(domain-specific presets)+ LLM-assisted role auto-gen 是 V0.7 主线 epic,体量与 V0.6.6 patch 节奏不匹配 |
| `ccteam migrate-from-claude`(反向 import 现有 `.claude/` 项目) | researcher R6#4 / codex-expert CX6#5 列入 V0.7+ backlog;依赖 `.claude/agents/*.md` 解析 + workflow.yaml 反推 + 用户交互 disambig,体量同上 |
| monorepo-aware `.mcp.json` | researcher R6#4;依赖 monorepo workspace 探测 + per-subtree `.mcp.json` 合并语义设计 |
| 国内 IM 启用(WeChat / 飞书 / DingTalk / QQ) | V0.7 Epic C(已在 V0.6.5 README §4 锁定);依赖各平台 SDK 接入 + Cargo features `lark` / `dingtalk` / `qq` / `wechat` 实装(目前 Cargo.toml 已占名,实现待写) |

---

## 5. Ship gate(V0.6.6 → main)

1. **baseline**:`cargo test --workspace --locked --no-fail-fast` ≥ **1660 / 1**(1583 起点 + ~80 新测试)
2. **clippy**:`cargo clippy --workspace --all-targets --locked -- -D warnings` 0 命中
3. **F166 GH Actions release CI 跑通**:tag `v0.6.6` push → 4-arch matrix(linux-x64 / macOS-arm64 / macOS-x64 / windows-x64)build artifact 上 GH Release,checksum 文件齐
4. **F166 install.sh 真验**:nas-box005(linux-x64)从 fresh wipe → `curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh` → `ccteam --version` 输出 `0.6.6` + PATH 写入正确;**手动签字** → `docs/versions/v0-6-6/host-probe.md`
5. **F167 sensible defaults host-probe**:fresh `/ccteam-creator "做个 monorepo 后端 reviewer"` → 生成的 workflow.yaml `scope:` 段非空、role.md 含与 monorepo 相关的 sensible default text;`/ccteam-creator "写个 TG 助理"` → chat-pocket preset + IM bot 段非空
6. **F168 实数 verify**:`grep -rnE "// TODO|// FIXME|// HACK|TODO\(" crates/*/src/` 输出 ≤ 2 个 site(剩下的必须是 V0.7 显式 defer-with-justification,**禁止**「未决」)
7. **F169 真 ledger 验**:nas-box005 daemon 跑一次 advise call → `@ccteam cost today` IM 回返值含真 USD 数字 + per-vendor 分项;CLI `ccteam-control show-cost` 输出同样数字
8. **F170 doc-comment scrub clean**:`grep -rn "V0.3.3 cleanup\|F49 wires\|once Wave 2 lands\|Wave 2 wires it into the" crates/*/src/` 0 命中
9. **F171 doctor stub-counter**:`cargo run --release -- doctor --verify-mcp` 输出含 `MCP tool surface: 26 active, 0 stubs`(沿用 V0.6.5 ship gate item #9 措辞)+ 失败 exit code = 1;**有自动化 test 覆盖**(`crates/ccteam-cli/tests/doctor_verify_mcp_test.rs`)
10. **F172 daemon-restart-recovery host-probe**:nas-box005 跑 mode-3 chat bot ≥10 turns → `kill -TERM <daemon pid>` → `ccteam start` 再起 → bot 自动 re-attach + 第 11 turn 输入能用得到前 10 turn 上下文(测试人确认 reply 引用早 turn 内容);progress.jsonl 含 ≥1 `chat_snapshot` event;**手动签字** → host-probe.md
11. **F173 Codex critic cost rollup 真验**:`mcp__ccteam__advise_vote` 调用 Claude+Codex 各一次 → `<root>/cost-budget.json` `advise_today_usd` 增加;`@ccteam cost today` 显示同样增量;`ccteam doctor` 不报 cost-orphan warning
12. **CLAUDE.md §一 baseline 表更新到 V0.6.6 数字**(test pass count + version + 当前最新版 + 上一版 + V0.6.x 延期候选)
13. **dev-coupling-audit.md 加 F166-F173 索引**
14. **F168 9 site 决断列表** 在 wave-handoff doc 留底(decided / EOL-deleted / V0.7-deferred-with-justification 三档分类)
15. **tag** `v0.6.6` push + GH Release notes(F166 binary download instructions 嵌入)

---

## 6. Wave 结构(patch 体量,1-2 wave)

详 `dev-plan.md`。概要:

```
Wave 0  doc-first(本 PR)
        ├── README.md           ← 本文件
        ├── prd.md              ← 8 finding 完整需求
        └── dev-plan.md         ← worktree-per-finding + acceptance gate

Wave 1  8 finding 全并行 worktree(每个独立 Opus subagent)
        F166 prebuilt binary + install.sh           ── 1.5 d
        F167 sensible defaults                       ── 1 d
        F168 active TODO sweep                       ── 1 d
        F169 cost_today ledger wire-up               ── 0.5 d
        F170 doc-comment scrub                       ── 0.5 d
        F171 doctor stub-counter                     ── 0.5 d
        F172 mode-3 context recovery                 ── 2.5 d
        F173 Codex critic cost rollup                ── 2 d

Wave 2  doc-syncer + host-probe + ship gate(必要时,可与 Wave 1 末段并行)
        CLAUDE.md §一 baseline 回填 + dev-coupling-audit.md 索引补
        nas-box005 真机跑 F166/F167/F169/F170/F171/F172/F173 host-probe,签字落
        docs/versions/v0-6-6/host-probe.md
        + workspace 0.6.5 → 0.6.6 + tag v0.6.6
```

每 Wave PR 必须 baseline ≥ 上 Wave 数字 + clippy 0 警告,否则不发。

**Strict no-wave-leftover**(沿 V0.6.5 加严)**:本版**不允许**把任何 finding 推到 V0.6.7 / V0.7。验收不过的 finding → 主会话 escalate 给用户 → 当场决策(继续做 / 主动 EOL 删除 / V0.7 显式 defer-with-justification),禁止 "TODO ship in V0.6.7"。

---

## 7. Doc-first 完成判据(本 PR 验收)

- [ ] `README.md`(本文件)落
- [ ] `prd.md` 8 finding 各章节完整(痛点 / 现状缺口 / 设计 / 文件 / 验收 / 风险)
- [ ] `dev-plan.md` 含 Wave 1 worktree 分配 + acceptance gate + Wave 2 doc-syncer 收尾
- [ ] CLAUDE.md `§一 当前状态` 表 `当前最新版` 行**暂不动**(代码 ship 后回填)
- [ ] 用户 review pass → merge → 新会话 8 worktree 并行 dispatch
