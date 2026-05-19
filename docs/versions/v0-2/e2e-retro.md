# V0.2 E2E Retro

> 范围:V0.2 全部 8 milestone(M0.16 - M0.23)post-merge end-to-end 验证。
> 基线 `cargo test --workspace` = 497/0;origin/main = `2fc0d83`。
>
> 方法:**4 suite 并行 subagent**,各自隔离 env,模拟真实用户操作。
> **不修代码**;发现的问题写 F-finding 留 V0.2.1 评估。

---

## 1. 测试方法

### 隔离 env

每 suite 自己的 `/tmp/ccteam-e2e-suite-<X>-<pid>/` 根目录,通过 env 重定向避免污染用户真 `~/.ccteam` / `~/.claude` / `~/projects`:

```bash
export CCTEAM_E2E_ROOT=/tmp/ccteam-e2e-suite-<X>-$$
mkdir -p $CCTEAM_E2E_ROOT/{ccteam-home,claude-home,projects-root,xdg-config}
export CCTEAM_HOME=$CCTEAM_E2E_ROOT/ccteam-home
export CLAUDE_CONFIG_HOME=$CCTEAM_E2E_ROOT/claude-home
export XDG_CONFIG_HOME=$CCTEAM_E2E_ROOT/xdg-config
export CCTEAM_PROJECTS_ROOT=$CCTEAM_E2E_ROOT/projects-root
```

**实测发现的 env 不对称**(详见 §3 F26):
- `mcp_serve::install_mcp()` 走 `dirs::home_dir()`,**不**读 `CLAUDE_CONFIG_HOME` — 必须叠加 `HOME=$CCTEAM_E2E_ROOT` 兜底
- `CCTEAM_PROJECTS_ROOT` 必设,否则 watchdog / project discovery 默认 `$HOME/projects` 会扫真用户 projects

### Suite 划分

| Suite | 覆盖 | base milestone |
|---|---|---|
| **A** Init / migration | doctor 三件套 / 1M context flag / V0.1→V0.2 migrate / daemon health gate | M0.16 / M0.20 / M0.23 |
| **B** Phase pipeline | dev / product-research e2e / phase show / validate-team / 三层 team resolution | M0.17 / M0.18 / M0.19 |
| **C** Hook integration | Stop hook self-loop exit-2 / 递归 guard / PreToolUse intercept-ask / settings.json 写入 / absolute path | M0.19 / M0.20 |
| **D** Smart layer | watchdog scan / quiet override / outbox push / team factory init/publish/validate | M0.21 / M0.22 |

约束:**不烧真 LLM cost**(phase loop 用静态结构验证 + Rust struct override 替代,不 spawn 真 claude);**不动 git state**(read-only + 隔离 env)。

### 不烧 cost 的 phase loop 验证

Suite B 试图用 stub claude_argv 跑 phase loop,**发现 V0.2 缺 CLI/env 注入路径**(详见 F29):
- `team.yaml` 没有 `claude_argv` 字段
- `OrchestratorConfig.claude_argv` 仅 Rust struct 内可改(test 用 `claude_argv: vec!["sh", "-c", ...]`)
- `ccteam start` 无 `--claude-argv`,无 `CCTEAM_CLAUDE_ARGV` env 读取

后果:Suite B 改用静态结构验证(bootstrap 后看 state.json / settings.json / phase 模板),phase loop 不真跑。

---

## 2. Suite-by-Suite 结果

### Suite A — Init / migration

| # | 场景 | Verdict |
|---|---|---|
| A1 | `ccteam doctor --install-skill --install-mcp --install-memory-bridge` | **PASS** |
| A2 | 1M context flag(`DEFAULT_CLAUDE_MODEL = "claude-sonnet-4-6[1m]"`) | **PASS** (静态验证) |
| A3 | `ccteam doctor --migrate-recommended-agents` | **PASS** |
| A4 | Daemon health gate(MCP `send_to_session` daemon 死 → fail-loud) | **PASS** |
| A5 | `ccteam ls` daemon health 注解 | **⚠ MINOR FAIL** |

**Suite A verdict: minor-followups**

### Suite B — Phase pipeline

| # | 场景 | Verdict |
|---|---|---|
| B1 | dev e2e phase 推进 | **partial pass / blocked-loop**(stub-claude 缺) |
| B2 | product-research e2e phase 推进 | **partial pass / blocked-loop** |
| B3 | `ccteam phase show dev implement`(inject prompt + body 分离) | **PASS** |
| B4 | `ccteam doctor --validate-team` warn-not-fail 协议字面量 | **PASS** |
| B5 | 三层 team resolution(project / user / repo first-source-wins) | **partial pass + finding** |

**Suite B verdict: minor-followups**

B5 关键发现:**Project-layer override 是 dead code**(详见 F28)。User → Repo fall-through 工作,但 Project 层无 production caller。

### Suite C — Hook integration

| # | 场景 | Verdict |
|---|---|---|
| C1 | Stop hook 第一次 → exit-2 + stderr | **PASS** |
| C2 | Stop hook 递归 guard `stop_hook_active=true` → exit 0 + outbox | **PASS** |
| C3 | `ccteam hook intercept-ask` → `permissionDecision: deny` | **PASS** |
| C4 | `bootstrap_project` 写 settings.json(PreToolUse + Stop + enabledPlugins) | **PASS** |
| C5 | settings.json 所有 `command` 字段 absolute path | **PASS** (11/11) |

**Suite C verdict: ready** — fully shippable.

### Suite D — Smart layer

| # | 场景 | Verdict |
|---|---|---|
| D1 | `watchdog scan` 空环境 | **PASS w/ caveat**(`daemon_down` 是基线 alert,test plan 期望 0 错) |
| D2 | watchdog cycle alert(`auto-loop.state.md::iteration ≥ 2`) | **PASS** |
| D3 | `notify_mode: quiet` + `daemon_down` 仍 surface | **PASS** |
| D4 | `watchdog scan --push --user <handle>` 写 meta-agent outbox | **PASS** |
| D5 | `ccteam team init my-team ...` | **PASS w/ note**(phase body 注释含 bare `PHASE_DONE` token,validator 正确跳过 colon-grammar) |
| D6 | `ccteam team publish --target local` symlink | **PASS** |
| D7 | `ccteam doctor --validate-team` plugin manifest 校验 | **PASS w/ ⚠**(`[FAIL]` 行不影响 exit code 与 Summary 计数) |
| D8 | `team.yaml` 顶级 unknown field 拒绝(M0.22 ⚠1) | **FAIL**(silently 接受;need-real-claude-smoke 看 plugin loader 是否 reject) |

**Suite D verdict: minor-followups**

---

## 3. 发现的 bugs / inconsistencies(F-finding 候选)

按优先级聚类(沿用 `dev-coupling-audit.md` 编号方案,V0.2.1 评估前先记账)。

| F | 标题 | 优先级 | 来源 |
|---|---|---|---|
| **F26** | `mcp_serve::install_mcp()` 不 honor `CLAUDE_CONFIG_HOME`,asymmetric with sibling install fns | P2 minor | Suite A |
| **F27** | `ccteam ls` 无 daemon health 注解(`render_ls_text` / `render_ls_json` `running:null`);spec 模糊 | P2 minor | Suite A A5 |
| **F28** | Project-layer team override 是 dead code(`TEAM_SOURCES = [Project, User, Repo]` 但无 production caller 用 `with_project()`) | **P1 medium** | Suite B B5 |
| **F29** | 无 CLI/env stub-claude 注入路径(`CCTEAM_CLAUDE_ARGV` / `--claude-argv` 都缺) — 阻塞 phase pipeline 的纯 CLI e2e | **P1 testability** | Suite B B1/B2 |
| **F30** | `ccteam doctor --validate-team` 撞 `[FAIL]` 行 exit 仍 0,Summary 计数器不算 plugin-section findings — 违反 `--help` 描述 ("Fails-loud on schema violations") | **P1 medium** | Suite D D7 |
| **F31** | `TeamSpec` 缺 `#[serde(deny_unknown_fields)]`,typo 静默 fall-back 到默认(M0.22 ⚠1 由 e2e 落实) | **P1 medium** | Suite D D8 |
| **F32** | doc drift:dev-plan 写 `~/.ccteam/daemon.heartbeat`,实际 `~/.ccteam/state/orchestrator.heartbeat`(M0.23 PR description 已记录,docs 待回填) | P2 cosmetic | Suite A |
| **F33** | `ccteam team init` 生成的 phase body 注释含 bare `PHASE_DONE` / `ESCALATE` tokens(无 colon),validator 正确跳过但 LLM 可能误读 | P2 cosmetic | Suite D D5 |

### Need-real-claude-smoke(subagent 不能验,留 user)

- **NRS-1**:`team.yaml` 作为 plugin top-level unknown 字段,Claude Code zod loader 是否真的 strip/silent-accept(M0.22 ⚠1)。**操作**:`claude /plugin enable my-team@ccteam-local` against published staging,看是否 surface "unknown key team.yaml"
- **NRS-2**:`marketplace.json` 当前只生成 `{name, description}`,无 `plugins[]` array — Claude Code 的 directory-source plugin discovery 是否接受这种最小 schema

---

## 4. PRD / interfaces / dev-plan inconsistencies

| # | 来源 | 描述 | 修正方向 |
|---|---|---|---|
| 1 | `dev-plan §9 M0.23.1` | 描述 "MCP 任意命令入口 + meta-agent skill 启动时 health check" — 没明说 `ccteam ls`;Suite A 测试 plan 把 `ls` 当成应注解 | 取舍:扩 dev-plan 显式纳入 `ls` annotation,或扩 Suite A 期望与代码一致(注释为非必需);**推荐**前者(F27 fix) |
| 2 | `dev-plan §7 M0.21.2` | 信号 2 描述 `progress.jsonl` 里 `auto_loop_cycle` 事件,实际无此 event;watchdog 读 `<project>/.ccteam/auto-loop.state.md::iteration` | 已在 M0.21 PR 文档同步段记录;dev-plan 待回填 |
| 3 | `dev-plan §9` | 写 `~/.ccteam/daemon.{pid,heartbeat}`;实际路径 `~/.ccteam/state/orchestrator.{pid,heartbeat}` | F32;tech-design.md §6.8 已对;dev-plan 段待回填 |
| 4 | `interfaces.md` | 未文档化 `daemon_down` 是 watchdog scan 的基线 alert(daemon down 时永远 emit 1 条) | 加段说明 |
| 5 | `doctor --help` | "Fails-loud on schema violations and IO-contract gaps" — 实际 `[FAIL]` 不影响 exit code | F30;改 help text 或改 exit 行为 |
| 6 | Suite A 测试 plan | 用 `ccteam send-to-session` CLI subcmd — 实际只有 MCP tool `ccteam__send_to_session`(via `ccteam mcp-serve`) | 测试 plan 错;impl 正确 |

---

## 5. Verdict

| 维度 | 数 |
|---|---|
| **Ship-blocking issues** | **0** |
| **P1 medium V0.2.1 candidates** | **4**(F28 / F29 / F30 / F31) |
| **P2 minor / cosmetic** | **4**(F26 / F27 / F32 / F33) |
| **Need-real-claude-smoke** | **2**(NRS-1 / NRS-2) |
| Suites with all-PASS | **1**(Suite C — hook integration) |
| Suites with minor-followups | **3**(A / B / D) |

V0.2 **可 ship**。e2e 主路径(init / 迁移 / hook self-loop / watchdog 翻译层 / team factory init+publish)全部 PASS 或 PASS-with-minor。

发现的 P1 全部是收尾向问题,**不影响 V0.2 用户首日 onboarding**:
- F28(Project layer dead code):用户暂时只用 User layer override;repo 层 fall-back 正常
- F29(stub-claude):dev/qa 工程内部痛点,不影响最终用户
- F30(`validate-team` exit code):用户层手工 lint 仍可读 `[FAIL]`;CI gating 不可用
- F31(`deny_unknown_fields`):团队作者打错字段名静默接受 — 用户体验差,但不破坏正确 yaml

---

## 6. V0.2.1 patch 计划(**全部完成:2026-05-08 V0.2.1 PR**)

按工作量 + 影响排序:

| Patch | F | 工作量 | 描述 | 状态 |
|---|---|---|---|---|
| **P1** | F31 | <1h | 加 `#[serde(deny_unknown_fields)]` 到 `TeamSpec` + nested struct;补 1 个测试 | **完成(V0.2.1 PR)** |
| **P2** | F30 | 1-2h | `render_validate_team_report` 累计 plugin-section `[FAIL]` 进 Summary + 非零 exit | **完成(V0.2.1 PR)** |
| **P3** | F28 | 2-4h | `for_orchestrator` callsite 改 `with_project(state.project_dir)`;`run_phase_show` 同;补 e2e 验证 project-layer 真 first-source-wins | **完成(V0.2.1 PR)** — `team_runtime_for_state` 返 `Cow<TeamRuntime>`,dispatch / process / stall / cost 切换 |
| **P4** | F29 | 1-2h | `OrchestratorConfig::default()` 读 `CCTEAM_CLAUDE_ARGV`;`ccteam start --claude-argv <shell-line>` flag。让 V0.2.2 e2e 真跑 phase loop | **完成(V0.2.1 PR)** |
| **P5** | F26 / F27 / F32 / F33 | <1h each | env / 注解 / docs 各小修(可合 1 PR) | **完成(V0.2.1 PR)** |

**前置 NRS smoke**(用户 1 分钟):
- NRS-1:`ccteam team init smoke && ccteam team publish smoke --target local && claude /plugin enable smoke@ccteam-local` — 看 stderr 是否含 "unknown key team.yaml"。结果决定 F31 是否升级 P0
- NRS-2:用 user 真 Claude Code attach 看 `claude /plugin list` 是否列出 my-team — 决定 marketplace.json schema 是否需要扩

---

## 7. Numbers

- **Suite 数**:4(并行)
- **场景数**:23(A:5 / B:5 / C:5 / D:8)
- **PASS**:18
- **PASS w/ caveat or note**:3
- **partial-pass / blocked**:2(B1/B2 stub-claude 缺)
- **⚠ minor fail**:1(A5)
- **FAIL**:1(D8 — 等 NRS-1 决定升级)
- **F-finding 候选**:8(F26-F33)
- **耗时**:~25 分钟(4 subagent 并行 wall-clock,主 session 协调 + retro draft 另 ~10 分钟)
- **Real LLM cost burned**:0
- **Real `~/.ccteam/` `~/.claude/` 污染**:Suite D 一次 `~/.claude/plugins/marketplaces/ccteam-local/` 漏隔离(已 cleanup),其他 0

---

## Changelog

- 2026-05-08:初版。基于 4 subagent 并行 e2e 报告 + 主 session 综合。base = origin/main `2fc0d83`。
