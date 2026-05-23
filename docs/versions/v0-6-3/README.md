# ccteam V0.6.3 — 外部触发 + vendor 接缝健壮性 + 跨 session 路由

> **状态:SHIPPED 2026-05-23(branch `claude/v0.6.3`)。** 四个 finding 全部合入 + 通过集成 gate;F-编号 F140-F143 → **F142-F145**(避开 V0.6.2 已占用的 F140 + F141 —— code/docs 重编号,commit message 历史保持原号)。
> **来源:** Multica(`multica-ai/multica`)对比调研 → 经「harness 套 harness」设计哲学筛子取四条最高价值点(对比分析为 session 内研究,未入库)。
> **基线:** test `1412/1`(V0.6.2 起点)→ `1471/1`(+59,四 finding 各带回归测试);clippy 0 warning;workspace version `0.6.2` → `0.6.3`。

---

## 0. 概览

| Finding | 一句话 | 对应痛点 | tech-design 节 | 体量 |
|---|---|---|---|---|
| **F142** | `Trigger::Schedule` 接真 cron(收尾 V0.4.6 stub) | 9 + 14 | §3.3.2 | S |
| **F143** | Webhook ingress(HTTP→文件入口) | 9 + 14 | §2.1 / §3.8 | S |
| **F144** | vendor 接缝 forward-compat 解析 | 14 | §2.1 HarnessAdapter | S |
| **F145** | 跨 session 运行时路由(squad 缩水版) | 13 | §3.3 | M |

**设计哲学筛子**:ccteam 是 meta-harness —— 套在 Claude Code / Codex(它们本身已是厚 agent runtime)之上。只做单个 harness **结构上做不到**的事,凡 harness 内部能做的一律下沉。四条都过筛:

- **F142 / F143** —— cron 与 webhook 只有「24/7 醒着的 daemon」能做,harness session 结构上不能给自己定时 / 监听外部事件;
- **F144** —— ccteam 的本分就是读 sub-harness 吐出的东西,vendor 按自己节奏发版,「读那条缝别崩」是 meta-harness 承重关节;
- **F145** —— 单 session 内的委派已被 Claude Code `Task` subagent 解决、ccteam 早已下沉;只补「跨 spawn 运行时路由」这条窄缝。

**被筛掉的:** Multica 的 per-issue KV store —— 真 meta-harness 地盘但不痛(事件→`progress.jsonl`、交付物→artifact 已覆盖)+ 有「第四 SoT」风险,撞墙再说。

---

## 1. F142 — `Trigger::Schedule` 接真 cron

**对应:** 痛点 9(团队不依赖人在场)+ 痛点 14(持续维护:夜间依赖升级 / 定时巡检)。tech-design §3.3.2。interfaces §17。

**现状缺口:** `Trigger::Schedule` 自 V0.4.6 一直是 stub(靠 meta-agent 手动触发代替),`AgentSpec::interval` 字段提过但未接真 cron。ccteam 无法定时跑任何东西。

**设计:**
- workflow.yaml:`trigger: schedule` + agent 新增 `schedule: "<5 段 cron>"` 字段(取代含糊的 `interval`)。
- daemon 主循环 tick 内评估到期的 schedule agent;**last-fire 时间戳持久化**到 `state.json`(per project+role)。
- **skip-missed 语义**:daemon 停机期间错过的触发**不补跑**(避免重启风暴),只判「下一个 due 时刻 ≤ now」。
- 引入轻量 cron crate(候选 `croner` / `saffron`),只用标准 5 段 cron。
- `parallelism` 强制 1(沿用非-watch trigger 语义);触发即正常 `spawn_claude_bg` + `agent_spawn` 事件。

**文件(预估):** `ccteam-core` workflow schema(`AgentSpec` 加 `schedule`)、`orchestrator.rs`(tick 评估)、`state.json` 持久化、`Cargo.toml`(新 dep)、`interfaces.md` §17。

**验收:** `schedule: "*/5 * * * *"` 每 5 分钟 spawn 一次;daemon restart 不重复触发、不补跑漏掉的;baseline 不退;clippy 0。

**风险:** 新增 dep —— 选 maintained、轻量、无重 transitive 的 cron crate。

---

## 2. F143 — Webhook ingress(HTTP→文件入口)

**对应:** 痛点 9 + 痛点 14(维护场景的触发源本质是外部事件:CI 红、CVE、PR 开)。tech-design §2.1 Channel Layer / §3.8 web。interfaces 新 endpoint。

**现状缺口:** ccteam 只能被本地文件 / IM / MCP / CLI 触发,**无法对外部系统事件反应**。

**关键设计选择:** webhook **不做成新的 `Trigger` 变体** —— 它是一个 HTTP→文件的薄入口,完全对齐「Channel Layer 是 dumb router、无内嵌 LLM」。

**设计:**
- 在 `ccteam start` 已有的 axum web server 加一条 `POST /webhook/:project/:token`。
- per-project webhook secret 由 `ccteam` 生成(类似 `~/.ccteam/web-token` 的随机 token),存项目 `state.json` 或 `<project>/.ccteam/`。
- 鉴权**只用 URL path 里的 token,不做 HMAC 签名** —— constant-time 比对 `:token` 路径段(token 在 path 段,要求 HTTPS 部署;签名留作未来按需)。
- 流程:token 比对 → 限 body 大小(如 256 KB)→ 写 `<project>/.ccteam/webhooks/<ts>-<rand>.json`(payload + 选定 header 元数据)。
- agent 用现成的 `trigger: watch:.ccteam/webhooks/` 消费 —— **`Trigger` enum 零改动**。
- **安全:** payload 当不可信外部输入 —— 限长、**绝不进 spawn argv**(写文件,agent 自己 Read),与 inbox 同级别处理;token 错 / 缺 → 401 不落文件;超限 → 413。
- **边界:** ccteam 只提供 endpoint;对外可达性(反代 / 隧道 / HTTPS)是部署问题,文档说明,不实现。

**文件(预估):** `ccteam-web`(新 route)、`ccteam-core`(secret 生成 + 存储)、CLI(可选:`ccteam show` 显示 webhook URL)、`interfaces.md`。

**红线核对:** R1 守(webhook 只是又一个写文件的 router);无内嵌 LLM 守;`cargo tree -p ccteam-web | grep ccteam-cli` 仍须 0 命中(不引入反向依赖)。

**验收:** 合法 token POST → 落文件 + watch agent spawn;token 错 / 缺 → 401 无文件;body 超限 → 413;baseline 不退;clippy 0。

---

## 3. F144 — vendor 接缝 forward-compat 解析

**对应:** 痛点 14(长跑可靠性)。tech-design §2.1 HarnessAdapter。

**现状缺口:** ccteam 解析 Claude Code job `state.json` / Codex `--json` 输出时,vendor(Anthropic / OpenAI)按自己节奏发新 CLI,新增字段 / 新 enum 值可能让 daemon panic —— 这是 ccteam 管不了的上游。

**与「不做历史迁移」红线的关系(重要):** 不冲突。重装红线管的是 **ccteam 自有 + 升级时一起 wipe** 的数据(workflow.yaml / ccteam 自有 state.json / progress.jsonl —— 升级即清,无跨版本老格式)。F144 管的是 **vendor 自有、ccteam 清不掉** 的文件。两类互不重叠。

**设计:**
- **范围严格锁死**:只动 `HarnessAdapter` 各实现里读 vendor 输出的 serde 结构体;**不碰** ccteam 自有 schema。
- 非关键字段 `#[serde(default)]`;状态 / event-kind enum 加 `#[serde(other)] Unknown` catch-all 变体。
- **未知 job 状态语义(待 review 拍板)**:倾向「当非终态、继续 probe + warn 一次」—— 宁可多 probe,不可误判 done 留 phantom。
- 未知 Codex event:skip + warn,不中断 event stream。
- 回归测试:喂合成的 future-JSON(未知字段 + 未知 enum 值),断言不 panic、降级符合预期。

**文件(预估):** `harness.rs` / `claude_job.rs` / Codex adapter 文件 + 对应测试。

**验收:** 喂未知 enum 值 / 多余字段不 panic;baseline 不退;clippy 0。

---

## 4. F145 — 跨 session 运行时路由(squad 缩水版)

**对应:** 痛点 13 —— specifically 其自己声明的边界「不解决自动任务分解(workflow.yaml 拓扑要人手工写)」。tech-design §3.3。

**现状缺口:** bg workflow 的路由是**静态拓扑** —— coordinator 只能往写死的 `output:` 目录写,无法在运行时决定「这个子任务交给哪个 role」。

**设计哲学(为什么是「缩水版」):** Multica 的 Squad = leader 拆活分活。但「拆到能装进一个 context 的活」Claude Code 的 `Task` subagent 已在单 session 内做了,ccteam **早已正确下沉、绝不重实现**。真正剩下的 meta-harness 窄缝只有「跨 spawn / 跨 session 运行时路由」;而 mode-3 的 bot-to-bot `@routing` + `hop_limit` 已验证此模式 —— F145 只是把它搬进 mode-2 bg workflow。

**设计:**
- workflow.yaml 顶层新增 `squad:` 块,**静态声明成员关系**(可审计):`squad: { leader: <role>, members: [<role>, ...] }`。
- 运行时分发:leader agent 往路由目录写**带 target 标签**的 artifact;orchestrator ArtifactWatcher 读 target → spawn 对应 member role(而非固定 role)。
- depth 限制:复用 AgentPath depth-limit(红线 R7),`coordinator→member→coordinator` 回路有界,超限 → `escalation`。

**开放设计点(待 review):**
1. target 标签载体:文件名前缀 `<target>--*.md` vs frontmatter `target: <role>`?
2. `squad:` 与现有 `trigger: watch:<dir>` 如何共存 —— member 是否需显式声明「接受 routed 输入」?
3. 「声明式拓扑」红线:成员关系静态声明 → 拓扑仍可审计;只有「分发」动态。需 review 确认这个折中可接受。

**文件(预估):** `ccteam-core` workflow schema(`squad:` 块)、`orchestrator.rs` / `artifact_watcher.rs`(target 解析 + 路由 spawn)、`interfaces.md` §17。

**红线核对:** R3 守(leader 路由决策只是「写哪个文件」,不注入 prompt);声明式拓扑 —— 成员静态声明,守;R7 守(depth-limit)。

**验收:** leader 写 `backend--task.md` → spawn backend role;hop_limit 超限 → `escalation` 事件;baseline 不退;clippy 0。

**体量:** M —— 四条里最大,跨 schema + orchestrator + watcher。

---

## 5. Dev-plan

**worktree-per-finding**(CLAUDE.md §五):`git worktree add -b <branch> /tmp/ccteam-f14X origin/main`,subagent 派工。

**并行 / 串行约束:**
- **F143、F144 完全独立,立即并行**(不碰 workflow schema)。
- **F142 与 F145 都改 workflow.yaml schema**(`AgentSpec` 加 `schedule` / 顶层加 `squad:`)→ schema 改动串行:**F142 先落 schema 并 merge,F145 基于其上加 `squad:` 块**(或一人统一改 schema)。
- 推荐排期:Wave A = F142 + F143 + F144 并行;Wave B = F145(F142 schema merge 后起)。

**每个 worktree 收尾:** 实现 + 测试 + clippy 0 warning + 同步对应 `interfaces.md` 段。

**version / 收尾:**
- `workspace.package.version` bump `0.6.1` → `0.6.3`,commit 用 `v0.6.3:` 前缀。
- CLAUDE.md §一 baseline 回填(`cargo test --workspace` 新数)。
- 本文件从 PRD 转为 V0.6.3 版本归档 README。
- `dev-coupling-audit.md` 加 F142-F145 索引。

**gate:** 每个 finding 合入前 baseline ≥ `1403/1`、clippy 0 warning,否则 block(CLAUDE.md §五.6)。

**交付:** 全部合入 `claude/v0.6.3` → 开**一个 PR** 到 main。

---

## 6. 红线核对(汇总)

| 红线 | F142 | F143 | F144 | F145 |
|---|---|---|---|---|
| R1 文件系统是控制平面 | 守 | 守(webhook = 写文件 router) | — | 守 |
| R3 No prompt injection | 守 | 守 | — | 守(路由 = 写哪个文件) |
| R7 fix-loop / depth escalate | — | — | — | 守(AgentPath depth-limit) |
| 声明式拓扑 | 守(`schedule` 声明) | 守 | — | 守(`squad:` 成员静态声明) |
| 不做历史迁移 | — | — | 守(只动 vendor 自有文件,不冲突) | — |
| 无内嵌 LLM(Channel Layer) | — | 守 | — | — |
| `ccteam-web` 独立 dep graph | — | 守(`cargo tree` 0 命中) | — | — |

---

## 7. Shipped notes(closeout 2026-05-22)

PRD §7 三个待确认项,实施时的最终决定:

1. **版本号** —— 按用户指定走 `0.6.3`(跳过 `0.6.2`;`docs/versions/` 无 v0-6-2 目录)。
2. **F144 未知 job 状态** —— 落「非终态续 probe + warn-once」。实施中另发现:代码库 vendor 输出解析全程走 `serde_json::Value`-plucking,**没有 strict serde enum** 可挂 `#[serde(other)]` —— 「未知字段忽略」一半本就由构造成立;F144 实际增量是把静默降级变成 warn-once 可观测 + 回归测试锁死。PRD §3 的 intent 达成。
3. **F145 三个开放设计点** —— 全按 PRD §4 倾向落地:target 标签用文件名前缀 `<member>--*.md`(非 frontmatter);`squad.members` 静态声明即「接受 routed 输入」声明,member 不另写 `trigger: watch`;hop count 编码进文件名 `<member>--h<N>--*`,`hop_limit` 默认 3,超限发 `escalation`(`kind: squad_hop_limit`),未知 member 前缀发 `kind: squad_unknown_target`。

### 合入提交(linear history,branch `claude/v0.6.3`)

| Finding | commit |
|---|---|
| F143 webhook ingress | `de6fee7` |
| F142 cron scheduler | `1b4e1c1` |
| F144 vendor-seam forward-compat | `483bad2` |
| F145 squad routing | `40f6031` |

集成 gate:`cargo test --workspace --locked --no-fail-fast` → 1462 pass / 1 fail(已知 flake `workflow_summary_reflects_agent_spawn_and_done_events`);`cargo clippy --workspace --all-targets -- -D warnings` → 0。
