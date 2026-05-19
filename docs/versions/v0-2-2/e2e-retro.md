# V0.2.2 E2E Retro

> 范围:V0.2.2 全部 7 PR(F34-F40)post-ship end-to-end 验证。
>
> base = `origin/main` `b651760`(V0.2.2 ship gate);测试 baseline 628/0。
>
> 方法:**4 suite 并行 subagent**,各自隔离 env。**不修代码**(F41 fix 在 retro 落档后单独 follow-up patch);发现的问题写 F-finding 留 V0.2.3 评估。

---

## 1. 测试方法

### 隔离 env

每 suite 自己的 `/tmp/ccteam-e2e-suite-<X>-<pid>/` 根目录,通过 env 重定向避免污染用户真 `~/.ccteam` / `~/.claude` / `~/projects`:

```bash
export E2E_ROOT=/tmp/ccteam-e2e-suite-<X>-$$
mkdir -p $E2E_ROOT/{ccteam-home,claude-home,projects-root,xdg-config}
export CCTEAM_HOME=$E2E_ROOT/ccteam-home
export CLAUDE_CONFIG_HOME=$E2E_ROOT/claude-home
export XDG_CONFIG_HOME=$E2E_ROOT/xdg-config
export CCTEAM_PROJECTS_ROOT=$E2E_ROOT/projects-root
export HOME=$E2E_ROOT
export CCTEAM_AUTO_SLUG=off  # 关 Tier 3,不烧 LLM cost
```

V0.2.1 retro F26 已修复(`mcp_serve::install_mcp` 现读 `CLAUDE_CONFIG_HOME`),
但 `font-kit` 等系统库仍按 `HOME` 查 `~/Library/Fonts`,所以 `HOME` 兜底必设。

### Suite 划分

| Suite | 覆盖 | base finding |
|---|---|---|
| **A** 命名 / CLI / migration | F39 cct convention sweep / F34 slug 4-tier / F40 team alias | F39 / F34 / F40 |
| **B** 控制层 | F35 silence classifier / F36 subagent guard | F35 / F36 |
| **C** prompt + 截图 | F37 决策树加固 / F38 终端截图 PNG | F37 / F38 / cct-project-creator skill |
| **D** 全栈 + 升级 | bootstrap pipeline / V0.1/V0.2 → V0.2.2 migration / docs 同步 + audit | 跨 finding |

约束:**不烧真 LLM cost**(`CCTEAM_AUTO_SLUG=off` + `--no-auto-slug` flag);
**不动 git state**(read-only + 隔离 env);**不动用户真 tmux**(suite C 用唯一前缀名 + 测后 kill 自己 session)。

---

## 2. Suite-by-Suite 结果

### Suite A — 命名 / CLI / migration

| # | 场景 | Verdict |
|---|---|---|
| A1 | `cargo build --release --bin cct` | **PASS**(`cct 0.2.2`,5 MB binary)|
| A2 | `cct doctor --install-skill` 装 3 skills | **PASS**(全有 marker + frontmatter)|
| A3 | F39 migration:managed legacy `ccteam-control/` 删 | **PASS** |
| A4 | F39 migration:user-edited legacy 保留 | **PASS**(after re-staging — 见 F43)|
| A5 | F39 migration:`settings.json` hook command rewrite | **PASS**(原子 + current_exe 路径)|
| A6 | F34 Tier 1 `--slug` B2 prefix | **PASS** |
| A7 | F34 Tier 1 verbatim 已带前缀 | **PASS** |
| A8 | F34 Tier 4 `slugify_brief` token 裁剪 | **PASS**(`Build a tiny Python CLI...` → `dev-build-tiny-python` 3 token)|
| A9 | F34 非法 slug 拒绝 | **PASS** |
| A10 | F40 alias warn + passthrough | **PASS**(state.team = `product-research` 字面入)|
| A11 | F40 canonical 不 warn | **PASS** |
| A12 | F40 alias 解析到 canonical via `--validate-team` | **FAIL**(F41) |
| A13 | F39 forward-looking docs sweep | **PASS**(0 命中;historical V0.1/V0.2 109 命中预期)|
| A14 | F39 templates 旧路径不存在 | **PASS** |

**Suite A verdict:minor-followups**(F41 P1 + F43 P2 docs nit)

### Suite B — 控制层

| # | 场景 | Verdict |
|---|---|---|
| B1 | `cargo test --workspace` baseline | **PASS**(628/0)|
| B2 | silence_classifier 7-class unit 全过 | **PASS**(21 unit)|
| B3 | pending_inject unit + e2e 全过 | **PASS**(10 unit + 8 e2e)|
| B4 | F35 enriched outbox 4 字段完整 | **PASS** |
| B5 | F36 pending-inject.json schema parse | **PASS**(serde round-trip)|
| B6 | F36 race coordination 测试 | **PASS**(`f36_race_miss_is_caught_by_f35_inject_limbo` + `f35_limbo_reinject_skipped_when_pending_inject_exists`)|
| B7 | F35 limbo retry counter cap = 1 | **PASS**(`MAX_LIMBO_RETRY=1`,2nd-tick `limbo_capped` outbox)|
| B8 | F35 capture_pane_tail with_ansi=false | **PASS**(纯文本入 outbox)|
| B9 | F38 capture_pane_with_ansi for PNG | **PASS**(独立函数,功能复用)|
| B10 | meta-agent 跳过 classifier(red line) | **PASS**(3 处 `is_evergreen` early-return) |
| B11 | F35 outbox 跟 V0.2 M0.19 L3 共存 | **PASS**(additive schema)|
| B12 | F36 max_defer_minutes default + timeout outbox | **PASS**(`DEFAULT_MAX_DEFER_MINUTES=10`,classification = `inject_defer_timeout`)|

**Suite B verdict:PASS**(0 finding)

### Suite C — prompt + 截图

| # | 场景 | Verdict |
|---|---|---|
| C1 | §1 决策树 "调研 X = 项目请求" 反例 | **PASS** |
| C2 | §3 克制规则 "❌ 不起 Agent 自调研" 反例 | **PASS** |
| C3 | §2 派单段改指 cct-project-creator skill | **PASS** |
| C4 | F39 cct binary 命名跟进 | **PASS**(`ccteam <cmd>` 0 命中,`cct <cmd>` 10+)|
| C5 | F40 research canonical 跟进 | **PASS**(`research 团队`/`--team=research` 4 hits;`product-research` 仅 alias 教育段)|
| C6 | skill 文件存在 | **PASS**(6.6 KB)|
| C7 | frontmatter `name: cct-project-creator` | **PASS** |
| C8 | ccteam-managed marker | **PASS**(begin/end 双标)|
| C9 | Phase A/B/C/D 段全在 | **PASS** |
| C10 | AskUserQuestion 用法 | **PASS**(9 处)|
| C11 | meta-agent context 强调 | **PASS** |
| C12 | F38 vendored TTF | **PASS**(`JetBrainsMono-Regular.ttf` 273.9 KB)|
| C13 | F38 LICENSES.md OFL | **PASS** |
| C14 | F38 ANSI_256 调色板完整 | **PASS**(256 项,test asserted)|
| C15 | F38 真 tmux 跑截图 | **PASS — 真 PNG 39329 字节,954 × 758 px 8-bit RGB** |
| C16 | F38 graceful degrade — tmux 失败 | **PASS-with-note**(exit 0 是设计,brief 期望错;详 §4)|
| C17 | F38 catch_unwind 防 panic | **PASS** |
| C18 | F38 MCP namespace 保留 ccteam | **PASS**(per PRD §8.3)|

**Suite C verdict:minor-followups**(F42 — skill body `product-research` 漂移)

### Suite D — 全栈 + 升级

| # | 场景 | Verdict |
|---|---|---|
| D1 | dev project bootstrap | **PASS**(state.json + settings.json + spec.md 全 well-formed)|
| D2 | settings.json hook commands 全 `cct hook` | **PASS**(11 commands,0 处 `ccteam hook`)|
| D3 | dev project state.json 字段 | **PASS** |
| D4 | research project bootstrap | **PASS** |
| D5 | product-research alias bootstrap + warn | **PASS**(slug + state.team 字面 passthrough)|
| D6 | doctor --install-skill 触发 migration | **PASS** |
| D7 | 老 ccteam-control(marker)被删 | **PASS** |
| D8 | 老 ccteam-team-author(frontmatter `name:`)被删 | **PASS** |
| D9 | 用户手改 `ccteam-handcrafted` 保留 | **PASS** |
| D10 | legacy settings.json hook command rewrite | **PASS**(absolute path + atomic + idempotent)|
| D11 | dev-coupling-audit.md F34-F40 全 close | **PASS**(7 条 section 全含 "已修复")|
| D12 | audit 计数到 35 | **PASS** |
| D13 | docs/versions/v0-2-2/ 完整 4 文件 | **PASS**(prd 67KB / dev-plan 53KB / README 1.9KB / feedback 4.5KB)|
| D14 | CLAUDE.md baseline 628 + version 0.2.2 | **PASS** |
| D15 | docs/README.md V0.2.2 行 + Patch 目录约定 | **PASS** |

**Suite D verdict:PASS**(0 finding) — fully shippable, no regressions.

---

## 3. 发现的 bugs / inconsistencies(F-finding)

按优先级聚类(沿用 `dev-coupling-audit.md` 编号方案):

| F | 标题 | 优先级 | 来源 |
|---|---|---|---|
| **F41** | `cct doctor --validate-team <alias>` resolve OK 后目录定位用原 alias 而非 canonical 误报 FAIL | **P1** | Suite A A12 |
| **F42** | `skills/cct-project-creator/SKILL.md` 5 处仍硬编 `product-research`,跟 F40 canonical `research` 漂移 | **P1** | Suite C |
| **F43** | PRD §8.2.5 detection signal 说明可补一句"测试 staging 模拟用户手改时需 strip both signals(marker + frontmatter `name:`)" | P2 docs nit | Suite A A4 |

### Need-real-claude-smoke(subagent 不能验,留 user)

无新增 NRS。F38 真 tmux 截图 Suite C 已通过;V0.1/V0.2 用户升级 migration Suite D smoke 已通过(simulated)。

---

## 4. PRD / interfaces / dev-plan inconsistencies

| # | 来源 | 描述 | 修正方向 |
|---|---|---|---|
| 1 | Suite C C16 brief 期望 | brief 写"tmux 失败 → exit code != 0",但 PRD §7.2.5 红线"rendering NEVER aborts the enclosing path"明文 graceful degrade 是退 0;brief 期望与设计冲突,**brief 错** | retro doc 内勘误,不算 finding |
| 2 | F43 — PRD §8.2.5 detection 双信号说明 | marker OR frontmatter `name:` 是 by design 的 OR 关系 — 测试 staging 模拟用户手改时需 strip both,PRD 应补一句免得 retro/test 设计者踩 | 改 PRD;P2 |

---

## 5. Verdict

| 维度 | 数 |
|---|---|
| **Ship-blocking issues** | **0** |
| **P1 V0.2.3 候选** | **2**(F41 / F42)|
| **P2 docs nit** | **1**(F43)|
| **Need-real-claude-smoke** | **0**(本轮全自跑覆盖)|
| Suites with all-PASS | **2**(B 控制层,D 全栈)|
| Suites with minor-followups | **2**(A 命名,C prompt + 截图)|

V0.2.2 **可 ship**(实际已 ship 在 main `b651760`)。e2e 主路径(命名约定 sweep / 控制层 classifier+guard / decision tree + 截图 PNG / 全栈 bootstrap + migration)全部 PASS 或 PASS-with-note。

发现的 P1 全部是收尾向问题,**不影响 V0.2.2 用户首日 onboarding**:

- F41(`--validate-team` alias 误报):用户走 canonical 名 `research` 完全 OK;走 alias `product-research` 时 doctor 报告说"FAIL"但实际 alias 解析正确,误导而非真 break;一行 fix
- F42(skill body `product-research` 漂移):派单到 `product-research` alias 仍 work(F40 alias 兼容),只是 skill `AskUserQuestion` 给用户的 label 老名;`AskUserQuestion` 内部一致性问题,不破坏功能

---

## 6. V0.2.3 patch 计划(若 ship 单独 patch)

按工作量 + 影响排序:

| Patch | F | 工作量 | 描述 |
|---|---|---|---|
| **P1** | F41 | <30 min | `commands.rs:1074-1078` `team` → `&spec.name`;加 `validate_team_resolves_alias_to_canonical_dir` 测试 |
| **P2** | F42 | <30 min | `skills/cct-project-creator/SKILL.md` 5 处 `product-research` → `research`;C段 default-toward 段加 alias 教育一句 |
| **P3** | F43 | <15 min | `docs/versions/v0-2-2/prd.md §8.2.5` 加测试 staging 双信号说明 |

**实际:F41+F42+F43 + 本 retro 文档** 同 PR 一波 ship(无须单独 V0.2.3 patch round;落 v0-2-2-retro-and-dust 单 PR)。

---

## 7. Numbers

- **Suite 数**:4(并行)
- **场景数**:59(A:14 / B:12 / C:18 / D:15)
- **PASS**:56
- **PASS w/ note**:2(C16 — exit code 期望与设计 / A4 — staging 双信号)
- **FAIL**:1(A12 — F41)
- **F-finding**:3(F41 P1 / F42 P1 / F43 P2)
- **耗时**:~30 min(4 subagent 并行 wall-clock,主 session retro 综合 + fix ~15 min)
- **Real LLM cost burned**:0(`CCTEAM_AUTO_SLUG=off` 关 Tier 3)
- **Real `~/.ccteam/` `~/.claude/` 污染**:0(全 isolated env;Suite C tmux 用唯一前缀名 + 测后 kill server)
- **PNG 截图实测**:39329 字节 / 954×758 8-bit RGB(F38 端到端跑通)

---

## Changelog

- 2026-05-09:初版。基于 4 subagent 并行 e2e 报告 + 主 session 综合 + F41/F42/F43 fix。base = `origin/main` `b651760`(V0.2.2 ship gate)。
