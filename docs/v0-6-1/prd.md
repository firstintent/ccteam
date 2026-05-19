# V0.6.1 — PRD

**12 finding(F98 + F119-F129),3 Epic 组织**:Epic D "Cleanup & honesty" / Epic E "User-claim 实现" / Epic F "Plan/Approval 落地"。详 `README.md`。

> **本 PR 范围**:本文件 + `README.md` + `dev-plan.md`(Wave 0 doc-first);代码改动全部走后续 Wave 1/2/3 PR。

---

## EPIC D — Cleanup & honesty(本版主题)

### F119 — probe script daemon-start + health-wait + mode-3 real round-trip

#### 痛点

`scripts/host-probe/run-probes.sh` V0.6.0 ship 时**不主动起 ccteam-imd daemon**,导致 Pocket Assistant / IM Squad 两 mode 3 场景真 TG round-trip 走不到 daemon;最终靠 manual curl + getUpdates 验证 TG channel surface OK。V0.6.0 ship 时 host-probe.md §Observations #1 已挂账。

#### 需求

`scripts/host-probe/run-probes.sh`:
- 前置:`ccteam-imd daemon start &` + `wait_for_imd_ready --timeout 30s`(health endpoint poll;ready 信号 = `~/.ccteam/imd/health` 文件 written + epoch ≥ start)
- 跑完 mode-3 scenarios 后:`ccteam-imd daemon stop` + verify exit code 0
- 中途崩:抓 daemon stderr → `.probe-results/<TS>/<scenario>/daemon-stderr.log`
- 新 env override:`CCTEAM_PROBE_SKIP_DAEMON_START=1`(允许 user 已自起 daemon)

`crates/ccteam-imd/src/daemon.rs`:
- 加 `health-write` 路径(daemon main loop 内 every tick `touch ~/.ccteam/imd/health`)
- 加 `health-check` CLI flag(`ccteam-imd health --timeout 30s` → exit 0/1)

#### 验收

1. `scripts/host-probe/run-probes.sh pocket-assistant` 自动起 daemon → 真 TG round-trip(user pre-`/start` bot)→ probe 通过(`cost.txt` 非零)→ 自动 stop daemon
2. `ccteam-imd health --timeout 5s` 不在 ready 状态 → exit 1
3. `CCTEAM_PROBE_SKIP_DAEMON_START=1` env 时跳过 start/stop(allowing user to manage own daemon)
4. nas-box005 跑完 8 场景 = 8 个 rc=0 + cost.txt 5+ 个非零(原 V0.6.0 全 0)

---

### F120 — overnight-builder probe full workflow

#### 痛点

`scripts/host-probe/run-probes.sh overnight-builder` V0.6.0 ship 时仅 `ccteam --help` smoke,**没真起 workflow**。host-probe.md §Observations #2 已挂账。

#### 需求

`scripts/host-probe/run-probes.sh overnight-builder`:
- 创 fake workflow:`/tmp/host-probe-overnight/workflow.yaml`(1 agent + 1 trigger + fake `<artifact_pattern>`)
- 创 fake artifact:`touch /tmp/host-probe-overnight/.ccteam/inbox/done.md`
- `ccteam start <slug>` + `wait_for_event --pattern agent_done --timeout 60s`
- assert `progress.jsonl` 含 `agent_spawn` + `agent_done`(jq grep)
- cleanup:`ccteam stop <slug>` + `rm -rf /tmp/host-probe-overnight/`

#### 验收

1. probe 跑完后 `cat /tmp/host-probe-overnight/.ccteam/progress.jsonl | jq -c '.event' | sort -u` 含 `agent_spawn` + `agent_done`
2. probe `rc=0` + `cost.txt` 非零
3. nas-box005 跑完 overnight-builder = rc=0 + 1 agent_done 事件

---

### F121 — `ccteam doctor --check-pricing-version`

#### 痛点

`crates/ccteam-cost/pricing/{anthropic,openai}.toml` 内有 `schema_version` + `pulled_from` + `pulled_at`(ISO date);V0.6.0 ship 时 anthropic 2026-05-19 / openai 2026-05-19。pricing 数据 3-6 月就漂;无机制提醒。Wave 1 V0.6.0 retained risk。

#### 需求

`crates/ccteam-cli/src/commands.rs`:
- 加 `--check-pricing-version` flag(可独立或并 `--check-codex-version` 等同跑)
- 实现:读两 toml 的 `pulled_at`,>180 天 = warn;>365 天 = error;<180 天 = ok
- 输出格式:`[pricing.anthropic] pulled 2026-05-19 (now -0d, OK)` / `[pricing.openai] pulled 2026-02-19 (now -90d, warn pricing aging)` / `[pricing.openai] pulled 2024-12-19 (now -365d, ERROR ship needs re-pull)`

`crates/ccteam-cli/tests/doctor_pricing_test.rs`(新):
- mock `chrono::Local::now`(用 `mockall` 或 `env CCTEAM_TEST_NOW=2026-08-19`)
- pin 3 个时间点 fail / warn / ok 分类

#### 验收

1. `ccteam doctor --check-pricing-version` 输出 2 行(anthropic + openai)
2. test mock 三状态分类正确
3. `ccteam doctor`(无 flag)隐式跑这个 + 其他 check
4. host probe nas-box005 跑 `ccteam doctor` 全绿(pulled_at 都 fresh)

---

### F122 — CodexAppServerAdapter notifications → progress.jsonl bridge

#### 痛点

`crates/ccteam-core/src/execution/codex_app_server.rs` notifications 当前只 SSE 端消费,没桥到 `progress.jsonl`。Wave 3 V0.6.0 D9 retained risk。**V0.6 mode 3 codex bot 未启,无 user-facing impact**,但 trait stack 一致性差;V0.7 启 codex bot 时同步加 bridge 是路径依赖,**本版按 honesty 原则提前补齐**。

#### 需求

`crates/ccteam-core/src/execution/codex_app_server.rs`:
- adapter 持 `Arc<ProgressJsonlWriter>`(同 `claude_tui.rs` 模式)
- 在 `events()` stream 翻译时,关键事件(turn_done / turn_started / error)写 progress.jsonl entry:
  ```json
  {"ts":..., "event": "turn_done", "vendor": "codex", "thread_id": "...", "cost_usd": ...}
  ```
- 跟 claude_tui 同样的 `translate_thread_event` 路径

`crates/ccteam-core/tests/codex_app_server_progress_bridge_test.rs`(新):
- mock UDS server emitting `turn/done` notification
- assert `progress.jsonl` 含对应 entry + vendor:codex
- assert cost 累加进 `cost_24h_by_vendor["codex"]`

#### 验收

1. 新 test pass
2. nas-box005 跑 `/ccteam-advise` Codex 路径 → `progress.jsonl` 含 `vendor:codex` 业务事件
3. 不破已有 claude_tui 路径(baseline test 全过)

---

### F123 — 5 demo GIF 录制

#### 痛点

V0.6.0 ship 时 5 preset demo GIF 全留空(`docs/v0-6-0/demos/.gitkeep`);root README L5 引 `30s-tg-bot-team.gif` = broken link。V0.6.0 wave-4 deferred。

#### 需求

`docs/v0-6-0/demos/`(注意:V0.6.0 dir,因为 demo 是 V0.6.0 ship 的 preset 演示;V0.6.1 是补)+ 5 个 GIF:
- `30s-solo-sidekick.gif`(`/ccteam "扫 TODO"` 30s 完整对话)
- `30s-team-sprint.gif`(`/ccteam-team 3 "fix TS errors"` 60s 浓缩)
- `60s-overnight-builder.gif`(`/ccteam-creator "夜里 qa-loop"` 60s 浓缩;含 TG 推送)
- `30s-pocket-assistant.gif`(TG DM 真录:user `今天 PR?` → bot 列表)
- `60s-im-squad.gif`(TG 群:user @ critic → critic @ fixer → fixer @ user)

录制工具:`asciinema rec` → `agg` 转 GIF;每个 ≤500KB(GitHub README 加载体感);分辨率 90×30 chars。

`docs/v0-6-0/demos/README.md`:
- recipe(精确命令)
- 录制 do/don't(prompt 不漏 token / 不录真 cost / 关 cursor blink)

#### 验收

1. 5 GIF 在 `docs/v0-6-0/demos/`(注意:V0.6.0 归档目录,V0.6.1 ship 补)
2. root README + docs/quickstart.md 引用全部有效(no broken)
3. 每个 GIF ≤500KB
4. demos/README.md 含 recipe 让别人能复刻

---

### F125 — 全局文档审计 + cross-doc 一致性 sweep + 历史归档清理

#### 痛点

- V0.5 → V0.6 改了多个核心 API(`spawn_session` → `start_thread` / cost crate 抽出 / mode 3 路径 flip),但 docs/ 根目录 tier-1 docs 可能仍引旧名(Wave 4 V0.6.0 sweep 不完整)
- 历史 v0-X-Y/ 归档自由生长;部分 prd.md / dev-plan.md 在 ship 后无用,占索引空间
- cross-doc 引用对仗散乱(`tech-design §N` vs `interfaces §N` 同主题指向不一)
- 术语漂移(mode 1/2/3 vs in-proc/bg/chat / "24 工具 5 group" vs 17 工具 / baseline 数字)

#### 需求

`doc-curator` teammate(Wave 1 跑 + Wave 3 finalize):

**1. Inventory + classify**:
```bash
find docs/ -name '*.md' | sort | xargs wc -l > .audit/doc-inventory.txt
```

分类:
- **tier-1 evergreen**(`docs/{README,requirements,tech-design,interfaces,dev-coupling-audit,orchestration-patterns,ccteam-as-domain-agnostic-orchestrator,claude-code-best-practices,claude-code-tool-surface}.md`)— 持续维护
- **user-facing**(`docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*`)— 持续维护
- **历史归档**(`docs/v0-X-Y/`)— 不动(per CLAUDE.md §五 "Pre-v1.0 不留技术债;EOL 内容去版本 dir")

**2. Drift detection grep(命中 0 才过)**:
```bash
# V0.5 旧 API
grep -rn 'spawn_session\b\|shutdown_session\b\|fn ingest_snapshot' docs/{README,tech-design,interfaces,orchestration-patterns,claude-code-tool-surface,ccteam-as-domain-agnostic-orchestrator}.md
# V0.4 phase 机制
grep -rn 'phase_state\|PhaseState\|pag\.rs\|subskill\.rs' docs/{README,tech-design,interfaces}.md
# V0.5 unprefixed MCP 名(Wave 4 sweep 漏的)
grep -rEn 'mcp__ccteam__(ls|show|peek|progress|new|pause|resume|send_to_session|inject_decision|spawn_agent|stop_agent|observe_agents|signal|set_parallelism|trigger_gate|get_artifact_summary)\b' docs/ skills/
# V0.5 cost / pricing 路径(已搬 ccteam-cost crate)
grep -rn 'ccteam_core::pricing\|ccteam_core::cost' docs/{README,tech-design,interfaces}.md
# V0.5 mode 3 路径(已 flip tmux 长跑)
grep -rn 'claude -p --resume\|stream-json + stdin pipe' docs/{tech-design,orchestration-patterns,interfaces,ccteam-as-domain-agnostic-orchestrator}.md docs/v0-6-0/
```

**3. Cross-doc 一致性**:
- 主 tier-1 docs 同一概念用同一术语:`HarnessAdapter` 5-method / `mode 1/2/3`(in-proc/bg/chat 描述一致)/ "24 工具 5 group"(本版完成后 26 工具 — 加 F128 2 个 + F129 1 个 + F124 plan-approval 路径 1 个 = 30 工具 5 group)/ baseline 数字(本版 ship 后)
- cross-doc 引用对仗:`tech-design §N` ↔ `interfaces §N` ↔ `CLAUDE.md §N` 同主题指同处

**4. CLAUDE.md baseline 同步**(Wave 3 finalize):
- §一 表格:Workspace version → **0.6.1**;baseline → 本版 ship 后数字;V0.6.0 → V0.6.1;V0.6.x 延期候选 list 更新(F98 / F119-F129 移到"已 ship",V0.7 列新 candidate)
- §三 红线表:加 row "**root README.md MUST be English**"(F126)

#### 验收

```bash
# baseline / clippy 持平
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy cargo test --workspace --locked --no-fail-fast 2>&1 | grep '^test result' | awk '{p+=$4;f+=$6}END{print p,f}'  # ≥ 本版 baseline / 1
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy cargo clippy --workspace --all-targets --locked -- -D warnings  # clean

# drift grep 全过(§2 listed greps 全 0 命中,tier-1 doc 内)

# CLAUDE.md baseline / version 持续 sync
grep -E '0\.6\.1' CLAUDE.md  # ≥1 hit
grep -E 'root README.md MUST be English' CLAUDE.md  # ≥1 hit(F126 红线)

# 历史 doc 索引在 docs/README.md 一致
grep -E 'v0-1|v0-2|v0-3|v0-4|v0-5|v0-6-0|v0-6-1' docs/README.md  # ≥7 hit(每版本 README link)
```

---

### F126 — README.md EN-only rewrite + CLAUDE.md 红线

#### 痛点

V0.6.0 root README 是 CN(`# ccteam` / `> **Claude Code 之上的 multi-agent 编排器** — 一个工具,三档能力 ...`)。OSS 项目主入口 EN 是普遍预期;国内用户面留 docs/quickstart.md + user-manual.md 等 CN。CLAUDE.md 无显式语言要求,易漂移。

#### 需求

**1. CLAUDE.md §三 红线表加一行**(本 PR 同步落):
```
| **root README.md MUST be English** | 守 | 守 | 守 |
```
配解释段(放红线表下):
> root `README.md` = OSS 主入口,必须英文。`docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*` 持续中文(国内用户面)。所有版本归档 `docs/v0-X-Y/` 中英不限(开发过程语)。

**2. root README.md EN 重写**:
- 保留 V0.6.0 改的 3-mode 平等 narrative(`mode 1 in-proc / mode 2 bg / mode 3 IM bot`)
- 保留 5 preset 表
- 保留三种对话入口表
- **删 "Status" / "V0.6.x 蓄势中" / "in production XXXX-XX-XX shipped" 任何版本/进展段** — 用户看的 README 是**始终呈现最新可用状态**的产品介绍,不夹版本进展。版本进展 / 候选 finding / 历史 ship 节点全部去 `docs/v0-6-1/README.md`(每版本独立)+ `docs/dev-coupling-audit.md`(F-finding 索引)
- README **footer 只允许 1 行**指向"What's new":`See [docs/v0-6-1/README.md](docs/v0-6-1/README.md) for current release notes`,但不展开内容
- 长度保持 ≤80 行(简洁 OSS landing,无 status 后更紧凑)

#### 验收

1. `head -3 README.md | grep -i 'claude code'` 命中且为英文 sentence
2. `grep -c '[一-鿿]' README.md` = 0(无中文字符)
3. `grep 'root README.md MUST be English' CLAUDE.md` 命中
4. `docs/quickstart.md` 等 user-facing CN 保留(retention check)
5. **`grep -iE 'status|shipped|baseline|in production|wave|finding|F[0-9]+|V0\.[0-9]+' README.md`** 命中 ≤2 行(只允许 footer 的 "What's new" link;无 status section)
6. **CLAUDE.md §三 红线表加 row** `| **README.md 不含版本进展/状态信息** | 守 | 守 | 守 |`(说明:版本进展去 `docs/v0-X-Y/README.md`,README 保持产品介绍)

---

### F127 — user-manual.md 100% 亲测可用 sweep

#### 痛点

V0.6.0 user-manual.md 写了用户面 slash / IM action 但**部分未实现**:
- `/ccteam-control change-persona helper-bot "..."` — L211(F128 落实)
- `/ccteam-control add-tool helper-bot "..."` — L212(F128 落实)
- `@ccteam pause helper-bot` IM NL admin — L227(F129 落实)
- `@ccteam cost today` — L228(F129 落实)
- `@ccteam stop everything` — L229(F129 落实)
- `/ccteam-doctor` — troubleshooting.md L1(实际是 CLI `ccteam doctor`,不是 slash)
- Cost auto-pause at 100% cap claim — L264(verify 已在 V0.6 implemented)

#### 需求

`manual-prover` teammate(Wave 3,F128 + F129 land 后跑):

**1. 逐节扫**(`docs/user-manual.md`)— 每节列 verifiable claims:
| § | Claim | 验证命令 | 期望 |
|---|---|---|---|
| §2.1 Solo Sidekick | `/ccteam "扫 TODO"` 自动路由 | 真在 Claude session 跑 + 看出错 fallback | 路由到 Solo Sidekick + agent done |
| §2.2 Team Sprint | `/ccteam-team 3 "fix TS errors"` 起 3 teammate | 跑 + 观察 fleetview | 3 teammate 出现 + lead |
| §2.3 Overnight Builder | `/ccteam-creator "夜里跑 qa-loop"` 自动判断 + 起 daemon | 跑 + 观察 progress.jsonl | workflow_started |
| §2.4 Pocket Assistant | TG DM bot → reply | TG 真消息 + `tail turns.jsonl` | bot reply 收到 |
| §2.4 | `/ccteam-control change-persona helper-bot "..."` | 跑 + verify `.claude/agents/*.md` 修改 | persona file changed |
| §2.4 | `/ccteam-control add-tool helper-bot "..."` | 跑 + verify workflow.yaml 修改 | tool added |
| §2.5 IM Squad | TG 群 @ critic → critic @ fixer | TG 真消息 + `turns.jsonl` | hop 计数 + escalation |
| §3.2 | `@ccteam pause helper-bot` | TG 群发 + verify workflow paused | workflow status = paused |
| §3.2 | `@ccteam cost today` | TG 群发 + verify reply | cost summary returned |
| §3.2 | `@ccteam list bots` | TG 群发 + verify reply | bot list returned |
| §3.2 | `@ccteam stop everything` | TG 群发 + verify | all workflow stopped |
| §3.3 Web 仪表板 | `http://localhost:7331` 看 workflow / 对话历史 / cost 趋势 | curl + screenshot | UI render |
| §4 Cost 透明 | 撞 90% TG 推送 | mock-inject + observe TG | TG message sent |
| §4 | 撞 100% 自动暂停 | mock-inject + observe | workflow paused + IM notify |
| §5 V0.5 → V0.6 升级 0 用户操作 | upgrade existing project | 跑 V0.5 backup → V0.6.1 → 跑 | no break |

**2. 修复 drift**(每行不通 = blocker;不"标 V0.7"):
- claim 未实现 → coord with Wave 2 teammate(F128/F129)refine 实现
- claim 实现差 → 直接 fix 实现(本 wave 内)
- claim 写错 / 措辞混淆 → 改 user-manual.md

**3. 产出**:
- `.audit/manual-prover-report.md`(每 claim 行 status + evidence path)
- `docs/user-manual.md` 改动 commit(若有 wording 调)
- 不删任何 claim(用户决定:build-everything)

#### 验收

1. `.audit/manual-prover-report.md` 每 claim 行 status ∈ {PASS,FIXED,N/A};没有 FAIL
2. user-manual.md 引用的所有 slash 在仓 `skills/*/SKILL.md` + MCP tool registry 内确认存在
3. `troubleshooting.md` `/ccteam-doctor` claim 修(改 `ccteam doctor` CLI 或新增 `/ccteam-doctor` slash)
4. nas-box005 host probe 跑完 § F127 验证表所有 claim

---

## EPIC E — User-claim 实现

### F128 — `/ccteam-control` 扩展 change-persona + add-tool subcommand

#### 痛点

user-manual.md §2.4 写 `/ccteam-control change-persona helper-bot "..."` + `/ccteam-control add-tool helper-bot "..."`,但 `skills/ccteam-control/SKILL.md` 不支持这两 subcommand,MCP `mcp__ccteam__admin_*` 工具集无对应。

#### 需求

**1. MCP 新工具(2 个)**:
- `mcp__ccteam__admin_change_persona` — 入参 `{slug, bot, new_persona_md}`(`new_persona_md` 是 NL 描述或完整 markdown);impl:读 `<project>/.claude/agents/<bot>.md`,LLM merge new_persona_md(skill 内做 prompt — daemon 不调 LLM),写回,emit `progress.jsonl::persona_changed`
- `mcp__ccteam__admin_add_tool` — 入参 `{slug, bot, tool_description}`(tool_description NL → workflow.yaml `tools:` 字段 append);impl:读 `workflow.yaml`,parse `tools:` list,append,写回,emit `progress.jsonl::tool_added`,bot 下次 turn 即 read 新 tool list

**2. `skills/ccteam-control/SKILL.md` 扩展**:
- 加 `change-persona <bot> "<NL description>"` 子命令(LLM 解读 NL → fill new_persona_md → 调 MCP)
- 加 `add-tool <bot> "<NL description>"` 子命令(NL → tool spec)

**3. crates/ccteam-cli/src/{commands.rs, mcp_admin_tools.rs}**:
- `change-persona` CLI 命令(走 MCP path)
- `add-tool` CLI 命令(同)

**4. tests**:
- `crates/ccteam-core/tests/admin_change_persona_test.rs`(新)— mock skill output,verify file diff
- `crates/ccteam-core/tests/admin_add_tool_test.rs`(新)— mock,verify workflow.yaml diff

#### 验收

1. MCP `mcp__ccteam__admin_change_persona` + `admin_add_tool` 注册 + schema 验证
2. `/ccteam-control change-persona helper-bot "改成英文 + 幽默"` → `.claude/agents/helper-bot.md` 改 + emit event
3. `/ccteam-control add-tool helper-bot "scan ~/Downloads"` → workflow.yaml `tools:` append + emit event
4. test 全过 + host probe nas-box005 跑通

---

### F129 — `@ccteam` IM NL admin via meta-agent

#### 痛点

user-manual.md §3.2 + V0.6.0 docs 多处写 IM 端 `@ccteam pause helper-bot` / `@ccteam cost today` / `@ccteam list bots` / `@ccteam stop everything` NL admin,但 ccteam-imd inbound router 不识别 `@ccteam` mention(只识别 `@<bot>` route 到 bot)。

#### 需求

**1. `crates/ccteam-imd/src/inbound.rs`**:
- 检测 `@ccteam <NL>` mention pattern
- 提取 NL,起短期 Claude `Task(subagent_type=ccteam-control)` 或调 meta-agent endpoint
- 把 admin action 翻译 → MCP `mcp__ccteam__workflow_pause / workflow_resume / workflow_ls / admin_stop_all`
- reply IM:执行结果(succeed/fail message)
- hop_limit 不消耗(meta-agent admin path 不算 bot-to-bot hop)

**2. meta-agent 翻译规则**(skill prompt 或直接 daemon-side keyword match — 简单 5 admin 走 keyword,复杂走 Task):
| NL | Action |
|---|---|
| `pause <slug>` | `workflow_pause` |
| `resume <slug>` | `workflow_resume` |
| `list bots` / `ls` | `workflow_ls` |
| `cost today` / `cost <slug>` | `cost_summary` |
| `stop everything` / `kill all` | `admin_stop_all`(危险,需 confirm second message)|

**3. tests**:
- `crates/ccteam-imd/tests/im_nl_admin_test.rs`(新)— mock TG inbound w/ `@ccteam pause helper-bot` → assert `workflow_pause` called
- 危险动作 confirm flow test(`stop everything` → "Are you sure? Reply CONFIRM"; only second message executes)

#### 验收

1. TG 群 `@ccteam pause helper-bot` → bot reply 确认 + workflow status = paused
2. `@ccteam list bots` → reply list with status
3. `@ccteam cost today` → reply summary
4. `@ccteam stop everything` → confirm prompt + 二次 CONFIRM 才执行
5. nas-box005 跑通 4 NL admin action

---

## EPIC F — Plan/Approval 落地

### F98 — plan-approval ↔ outbox 联动

#### 痛点

V0.5 长跑 workflow 写 plan 但 user 没 IM 通道审批 → 长跑任务夜里改方向无落点。V0.5 deferred,V0.6 PRD §八 retained,V0.6.1 闭环。

#### 需求

**1. workflow.yaml schema 扩展**:
```yaml
agents:
  reviewer:
    plan_approval:
      enabled: true
      outbox: telegram     # use registered IM transport
      timeout_min: 60      # if no approval in 60 min, auto-reject or escalate
      on_timeout: escalate # | auto-approve | reject
```

**2. orchestrator plan-approval flow**:
- agent writes `<project>/.ccteam/plans/<agent>-<ts>.md` plan
- orchestrator detect plan write(artifact watcher) + agent paused state
- 调 ccteam-imd 发 IM message: `[<workflow>] <agent> wrote plan:\n<head -20 plan>\n\nReply APPROVE or REJECT in 60min.`
- user 回 `APPROVE` / `REJECT` / `EDIT <comment>` IM → ccteam-imd parse + emit `plan_decision` event → orchestrator resume agent + inject decision

**3. F124 narrow scope merge**:
- HITL = plan-approval 流的实现细节,本 finding 包含 F124 大部分
- F124 单算"6 号编排模式 mode key"那部分(workflow.yaml `mode: human-approval`,与现 3 mode 并列)

#### 验收

1. workflow.yaml 含 `plan_approval` block → daemon parse + watch plan path
2. agent 写 plan → IM 内收 `APPROVE/REJECT` prompt
3. IM 回 `APPROVE` → orchestrator inject decision + agent resume
4. 60min 无 reply + `on_timeout: escalate` → emit `plan_timeout` + bot ping
5. test: `crates/ccteam-core/tests/plan_approval_test.rs`(新)mock IM,verify full loop

---

### F124 — HITL Approval Gating(narrow scope)

#### 痛点

V0.6 PRD §八 deferred 项;V0.6.1 按"遗留全完成"硬约束 ship 但 narrow scope = workflow.yaml `mode: human-approval` 接入(与现 in-proc / bg / chat 3 mode 并列)。

#### 需求

**1. workflow.yaml `mode: human-approval`**:
- 第 4 个 mode(并 in-proc / bg / chat)
- 行为:每个 agent step 走 plan-approval(F98)流程 — 不自动 spawn 下一个 step,等 IM `APPROVE`
- 用例:critical workflow(大规模 refactor / migration)半夜 user 不在 = workflow 自然 hold,user 早晨 IM 一个 APPROVE 串起一整天 progress

**2. orchestrator mode dispatch**:
- `pick_adapter` + state machine 加 human-approval mode 分支
- mode 不与 bg/chat 互斥:bg + human-approval = "bg workflow,每 step approve"
- chat + human-approval = "chat bot,每 reply approve"

**3. 红线**(CLAUDE.md §三 加 row):
```
| **HITL approval state SoT** | — | progress.jsonl::plan_decision | 同 |
```

#### 验收

1. workflow.yaml `mode: human-approval` parse 不 error
2. agent step done → orchestrator hold + emit IM approval prompt
3. IM `APPROVE` → resume next step
4. 红线 row 入 CLAUDE.md
5. F127 host probe 跑 1 个 human-approval mode workflow + IM round-trip

---

## 各 finding 文件清单概览(详 dev-plan.md 内)

| F | 主要新 files | 主要修改 files |
|---|---|---|
| F119 | — | `scripts/host-probe/run-probes.sh` / `crates/ccteam-imd/src/daemon.rs` |
| F120 | — | `scripts/host-probe/run-probes.sh` |
| F121 | `crates/ccteam-cli/tests/doctor_pricing_test.rs` | `crates/ccteam-cli/src/commands.rs` |
| F122 | `crates/ccteam-core/tests/codex_app_server_progress_bridge_test.rs` | `crates/ccteam-core/src/execution/codex_app_server.rs` |
| F123 | `docs/v0-6-0/demos/*.gif` + `README.md` | `README.md`(root)/`docs/quickstart.md` |
| F124 | — | `crates/ccteam-core/src/orchestrator.rs` + `workflow.rs` + `CLAUDE.md` |
| F125 | `.audit/doc-inventory.txt` | tier-1 doc sweep |
| F126 | — | `README.md`(root EN rewrite)+ `CLAUDE.md` |
| F127 | `.audit/manual-prover-report.md` | `docs/user-manual.md` / `docs/troubleshooting.md` |
| F128 | `crates/ccteam-core/src/mcp_admin_tools.rs`(扩)+ 2 test files | `skills/ccteam-control/SKILL.md` |
| F129 | `crates/ccteam-imd/tests/im_nl_admin_test.rs` | `crates/ccteam-imd/src/inbound.rs` + meta-agent skill |
| F98 | `crates/ccteam-core/tests/plan_approval_test.rs` | `crates/ccteam-core/src/orchestrator.rs` + `workflow.rs` |

---

详 `dev-plan.md`(wave teammate briefing + acceptance gate)。
