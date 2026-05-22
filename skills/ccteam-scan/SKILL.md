---
name: ccteam-scan
description: "大型代码库导航性体检 —— 只读 audit。探测 monorepo 结构、为每个子系统建议 workflow.yaml 的 `scope:` 值、报告 navigability gap(分层 CLAUDE.md / permissions.deny 噪声排除 / LSP 缺失)。Use when 用户说 '/ccteam-scan'、'检查我的大代码库'、'扫一下这个仓库'、'我的 monorepo 怎么接 ccteam'、'workflow.yaml 的 scope 该填什么'、'audit codebase navigability'。V0.6.2 F141。"
---

# /ccteam-scan — 大型代码库导航性体检

V0.6.2 F141。**只读** audit skill。一次性产出一份报告:这个仓库对 Claude Code agent 有多"可导航",以及把它接入 ccteam 时每个 role 的 `scope:` 该怎么填。

灵感来源:Anthropic《How Claude Code works in large codebases》—— "Claude 的能力 = 找到正确 context 的能力;太多则退化,太少则盲目"。本 skill 是 `docs/orchestration-patterns.md §1.5` explorer→artifact→editor 模板里 `explorer` role 的一次性交互版。

## skill 家族(本 skill 所处位置)

| 用户意图 | Skill |
|---|---|
| 总入口 NL dispatcher | `ccteam` |
| 起临时 team 干活(in-session 多 teammate) | `ccteam-team` |
| 起新项目 / workflow / IM bot | `ccteam-creator` |
| 管理已有 ccteam 项目 | `ccteam-control` |
| 一次性 IM token 绑定 | `ccteam-im-setup` |
| Claude + Codex 并行 advisor | `ccteam-advise` |
| **大型代码库导航性体检(本 skill)** | **`ccteam-scan`** |

## When to invoke

触发短语(LLM 语义匹配,无需 regex):

- `/ccteam-scan [路径]` — 显式 slash 入口(路径省略 = 当前 git 仓库根)
- "检查 / 扫一下我的大代码库"、"这个 monorepo 怎么接 ccteam"
- "workflow.yaml 的 `scope:` 该填什么"
- "我的仓库对 agent 友好吗 / navigability audit"

不要在以下场景触发:

- 用户要的是代码 review / bug 排查 —— 那是普通编码任务,不是导航性体检
- 仓库很小(`git ls-files | wc -l` < ~200)—— 直接告诉用户"小仓库不需要 scan,一份 root CLAUDE.md 足够",不跑完整流程

## 核心原则 —— 只读 advisory

ccteam 是 outer harness;`CLAUDE.md` / LSP / hooks / `.claude/settings.json` 属于 inner harness(Claude Code)+ 项目仓库。**本 skill 只读、只报告、只建议,绝不代为修改**:

- ✅ 读源码、跑 `git` / `find` / `rg` / `du` 统计、读已有 `CLAUDE.md` / `settings.json`
- ✅ 唯一写动作:把报告落到 `<repo>/.ccteam/codebase-scan.md`(ccteam 控制平面;不是源码、不是配置)
- ❌ 不写 / 不改 `CLAUDE.md`、`.claude/settings.json`、`workflow.yaml`、任何源文件
- ❌ 不装 LSP plugin、不生成 `CLAUDE.md` 内容

发现的 gap 一律以"建议"呈现,由用户(或后续 `/ccteam-creator`)决定是否采纳。

## Step 1 — 规模与形状

在仓库根跑(全部只读):

```bash
git rev-parse --show-toplevel                       # 锁定仓库根
git ls-files | wc -l                                # 受管文件数
git ls-files | sed 's#/.*##' | sort -u              # 顶层目录
git ls-files | awk -F. 'NF>1{print $NF}' | sort | uniq -c | sort -rn | head  # 语言分布
du -sh -- */ 2>/dev/null | sort -rh | head -15      # 最大子树
```

判定:文件数 > ~2000 或顶层目录 > ~15 → 视为"大型",继续;否则给"小仓库无需 scan"结论收尾。

## Step 2 — monorepo 结构探测

按存在性探测 workspace marker,列出所有 member:

| marker 文件 | 生态 | member 来源 |
|---|---|---|
| `Cargo.toml` 含 `[workspace]` | Rust | `members = [...]` |
| `pnpm-workspace.yaml` | pnpm | `packages:` glob |
| `package.json` 含 `workspaces` | npm / yarn | `workspaces` glob |
| `nx.json` / `lerna.json` | Nx / Lerna | `projects` / `packages` |
| `go.work` | Go | `use (...)` |
| `WORKSPACE` / `MODULE.bazel` | Bazel | `BUILD` 文件所在目录 |
| `settings.gradle[.kts]` | Gradle | `include(...)` |
| `pom.xml` 含 `<modules>` | Maven | `<module>` |

无 marker 但顶层目录多 → 按目录形状(每个顶层 dir 是否有自己的 build 文件 / `src/`)推断"事实 monorepo"。

## Step 3 — `scope:` 建议(本 skill 的核心产出)

对每个 member / 主要子系统,给出一行可直接粘进 `workflow.yaml` 的 `scope:` 值。背景:V0.6.2 F140 —— `AgentSpec.scope` 把 agent spawn 的 cwd 钉到子树,收窄每次 fresh-context 的爆炸半径(详 `docs/interfaces.md §17.2`)。

输出形如:

```
子系统               建议 scope:                理由
services/payments    scope: services/payments   独立 pnpm package,有自己的 test
crates/api           scope: crates/api          Cargo workspace member
(跨子系统的 role)     (省略 scope = 项目根)       需要全仓视野的 role 不设 scope
```

原则:**一个 role 只该看它要改的子树**;需要全仓视野的(如 explorer / 架构审查)才留空 scope = 项目根。注意 `scope` 必须是相对路径且不含 `..`(否则 `WorkflowSpec::validate` 拒绝)。

## Step 4 — navigability gap 体检(advisory)

只报告,不修复:

1. **分层 CLAUDE.md** —— 有 root `CLAUDE.md` 吗?大型 member 有没有自己的 subdir `CLAUDE.md`?缺 → 建议用户(或 `/ccteam-creator`)为每个 member 补一份(含该目录 test / lint 命令)。**本 skill 不代写内容。**
2. **噪声排除** —— `.claude/settings.json` 的 `permissions.deny` / `.gitignore` 有没有挡掉 `target/` `node_modules/` `dist/` `build/` 生成代码 / vendored 依赖?缺 → 建议补 committed deny 规则。
3. **LSP / 符号导航** —— 检测到的语言(尤其 C / C++ / C# / Java / Rust / TS)有没有对应 code-intelligence plugin?缺 → 建议用户装(本 skill 不装)。grep 一个常见函数名返回上千 match 烧 context,LSP 按 symbol 过滤。
4. **codebase map** —— 顶层目录 > ~30 且无导航文档 → 建议加一份 root 目录 table-of-contents。

## Step 5 — 产出报告

把 Step 1-4 汇成一份 markdown,写到 `<repo>/.ccteam/codebase-scan.md`,结构:

```markdown
# Codebase scan — <repo 名> (<日期>)
## 规模           <文件数 / 顶层目录 / 语言 top-3 / 最大子树>
## monorepo 结构   <生态 + member 列表>
## scope 建议      <表:子系统 → scope 值 → 理由>
## navigability gap  <分层 CLAUDE.md / 噪声排除 / LSP / map 四项,各标 ✅ / ⚠️>
## 下一步          接 /ccteam-creator 用上面的 scope 建议生成 workflow.yaml
```

同时在对话里给用户一段 ≤10 行的人话摘要 + 最关键的 1-2 条建议。报告文件就是 explorer→artifact→editor 模板里的 explorer artifact —— `/ccteam-creator` 可直接读它生成 workflow.yaml。

## What this skill does NOT do

- 不改任何源码 / 配置 / workflow.yaml —— 只读 + 只写 `.ccteam/codebase-scan.md` 报告
- 不代写 `CLAUDE.md` 内容、不装 LSP —— 那是 inner harness / 项目仓库职责
- 不跑 agent、不 spawn session —— 纯一次性 audit
- 不做代码质量 / 安全 review —— 那是别的 skill / team 的事

## Red lines

- **只读** —— 除 `<repo>/.ccteam/codebase-scan.md` 报告外,零写动作
- **advisory** —— 所有 gap 是建议,采纳与否由用户决定
- ccteam 只吸收"拓扑"这一条 —— scan 报告 inner-harness gap,但不拥有 / 不修复它

## Where to look in the repo

- `docs/orchestration-patterns.md §1.5` —— 大型代码库模板(scope + explorer→artifact→editor)
- `docs/interfaces.md §17.2` —— `AgentSpec.scope` schema
- `docs/versions/v0-6-2/README.md` —— F140 per-role scope + F141 本 skill
