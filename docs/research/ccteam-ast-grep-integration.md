# ccteam 集成 ast-grep 架构分析

面向读者：ccteam 架构师、核心开发者、team/phase 作者  
研究对象：[`ast-grep/ast-grep`](https://github.com/ast-grep/ast-grep) 及其官方文档、`ast-grep-mcp`、Claude Code / Codex 工具面  
上下文依赖：

- [`architecture-analysis.md`](architecture-analysis.md)：ccteam 当前 Thin Harness、文件协议、phase DAG、hooks 与 tool surface 边界。
- [`thin-harness-fat-skills-architecture-improvement.md`](thin-harness-fat-skills-architecture-improvement.md)：Fat Skills、Tokenmaxxing、review/QA gate 的演进方向。
- [`ccteam-codex-integration.md`](ccteam-codex-integration.md)：Codex 作为控制端、review/QA sidecar 与未来 worker provider 的路径。

结论先行：**ast-grep 值得集成，但不应作为 Claude Code / Codex 的替代品，而应作为结构化代码检索、项目级规则、codemod 和 review evidence 的确定性工具层。** 它对 ccteam 的增强是明显的，但主要集中在“结构化搜索 / 批量改写 / 可复用规则 / review gate 证据化”这几类任务；对小型一次性任务、纯文本搜索、类型语义分析和运行时行为判断提升有限。

---

## 1. ast-grep 是什么

ast-grep 是一个基于 tree-sitter AST 的 CLI 工具，用于：

- 结构化搜索：用“像代码一样”的 pattern 匹配语法结构。
- lint：用 YAML rule 定义项目级规则。
- rewrite/codemod：按 AST pattern 做安全得多的批量改写。
- 多语言扫描：官方文档列出 20+ 内置语言，并支持自定义 tree-sitter parser。
- AI 集成：官方文档提供 Claude Code skill、AGENTS.md prompt、MCP server、LLM 优化文档包等路径。

与 `rg` / `grep` 的本质区别：

```text
rg 找文本
ast-grep 找语法结构
```

示例：

```bash
ast-grep -p '$A && $A()' -l ts -r '$A?.()'
```

这个 pattern 表达的是“同一个表达式先 truthy 检查再调用”，比文本正则更接近开发者意图。

---

## 2. 为什么它适合 ccteam

ccteam 的核心架构是：

```text
Rust thin harness
  → 调度 phase / 状态 / 文件协议 / hooks
Claude Code / Codex agents
  → 规划 / 实现 / review / QA
Fat Skills / phase markdown
  → 复杂经验、工作流和判断标准
```

ast-grep 的位置正好填补一个空缺：**LLM 能提出“我要找什么结构”，ast-grep 能确定性地找到“代码里有没有这种结构”。**

这对 ccteam 有四个价值：

1. **把 review 从纯主观变成带证据**  
   Codex 或 Claude 说“可能有 unsafe unwrap”，ast-grep 可以列出确切位置。

2. **让 Tokenmaxxing 更精确**  
   不需要把全仓喂给 agent；先用 ast-grep 找结构性候选，再把少量相关文件喂给 phase。

3. **把经验沉淀成 rule pack**  
   retro 中的“不要再做 X”可以转成 `.ccteam/ast-grep/rules/*.yml`，成为未来项目的 gate。

4. **让 codemod 自动化可控**  
   LLM 负责设计 rewrite 规则，ast-grep 负责批量匹配和替换，测试负责验收。

---

## 3. 与 Claude Code / Codex 自带工具的对比

### 3.1 能力矩阵

| 能力 | Claude Code / Codex 自带工具 | ast-grep 增强 |
|---|---|---|
| 文本搜索 | `Grep` / shell `rg` / Codex shell | 无明显增强，`rg` 更直接 |
| 文件浏览 | `Read` / `Glob` / shell | 无明显增强 |
| 语义解释 | LLM 上下文理解 | ast-grep 不做语义解释 |
| 结构搜索 | 依赖 LLM 写 shell/rg/脚本，容易漏边界 | 明显增强：AST pattern |
| 批量 rewrite | LLM 编辑多个文件，风险高 | 明显增强：结构化 codemod |
| 项目自定义 lint | 依赖 prompt 或人工 review | 明显增强：YAML rules 可复用 |
| review evidence | LLM 主观描述 + 测试输出 | 明显增强：规则命中列表 |
| 类型/数据流分析 | 编译器、linter、TypeScript/Rust 工具链 | ast-grep 不替代 |
| 安全深度分析 | LLM + 专业工具 | ast-grep 只适合语法级 anti-pattern |
| 大仓库探索 | LLM 容易上下文爆炸 | 明显增强：先检索再喂 context |

### 3.2 与 `rg` 的边界

继续用 `rg` 的场景：

- 找字符串、配置项、文件名、日志文本。
- 找函数名、类型名、常量名。
- 简单 grep 足够准确的搜索。

改用 ast-grep 的场景：

- 找“某类语法结构”，例如所有 `unwrap()` 调用。
- 找“上下文约束”，例如 React component 内直接调用某 hook。
- 找“形状相同但变量名不同”的代码。
- 批量 rewrite 且希望避开字符串和注释误伤。
- 把 review 发现固化成可重复扫描的 rule。

### 3.3 与编译器 / linter / Semgrep / CodeQL 的边界

ast-grep 不应替代：

- `cargo check` / `tsc` / `go test` 等类型和编译检查。
- ESLint / Clippy / Ruff 等生态 linter。
- Semgrep / CodeQL 等安全和数据流分析工具。

它适合做“轻量 AST 规则层”：

- 比 `rg` 更懂代码结构。
- 比写 tree-sitter 程序更低成本。
- 比完整静态分析工具更轻、更容易由 agent 临时生成。

---

## 4. 能给 Claude Code 带来什么效果

### 4.1 plan-eng 阶段：结构化 context pack

当前 `plan-eng` 要求读 spec、产出架构和计划。引入 ast-grep 后，可以增加一个轻量 “structural reconnaissance” 步骤：

```text
plan-eng
  → 用 rg 找候选模块
  → 用 ast-grep 找结构模式
  → 产出 .ccteam/ast-grep/context.md
  → architecture.md 引用结构证据
```

例子：

- Rust 项目：找所有 `tokio::spawn`、`unwrap()`、`std::process::Command`。
- TypeScript 项目：找所有 React component、hook 调用、fetch/axios 调用。
- Python 项目：找所有 FastAPI route、SQL query、subprocess 调用。

收益：

- Claude 不必靠全文阅读猜架构。
- context 更小、更准。
- plan 中的影响范围可被证据支撑。

### 4.2 implement/fix 阶段：安全 codemod

Claude Code 可以生成 ast-grep rewrite，但 ccteam 应强制流程：

```text
1. 先 search，不改文件
2. 输出命中列表
3. 用户或 phase 确认 rewrite
4. 应用 rewrite
5. 展示 diff
6. 跑测试
```

禁止默认直接跑 destructive rewrite。尤其不能让 phase prompt 写：

```bash
ast-grep ... -r ... -i
```

除非该 phase 明确是 codemod phase，并且前面已经有 dry-run artifact。

### 4.3 review 阶段：把主观判断变成 gate

当前 `implement` 已支持 code-reviewer sub-skill。ast-grep 可以补一个确定性 layer：

```text
Claude reviewer / Codex reviewer
  → 读 plan 和 diff
  → 生成或选择 ast-grep rules
  → ast-grep scan
  → review 结论带命中证据
```

产物：

```text
.ccteam/ast-grep/findings.json
.ccteam/ast-grep/report.md
.ccteam/codex-review.md
```

这会让 review 从“我觉得这里可能有问题”变成“规则 X 命中 7 处，文件和行号如下”。

---

## 5. 能给 Codex 带来什么效果

根据 `ccteam-codex-integration.md`，Codex 在 ccteam 里的近期定位是 review/QA sidecar，而非主 phase worker。ast-grep 很适合给 Codex sidecar 提供确定性工具。

### 5.1 Codex review sidecar + ast-grep

建议流程：

```text
implement phase_done
  → codex exec --sandbox read-only
  → Codex 读取 plan / architecture / diff
  → Codex 编写或选择 ast-grep patterns
  → Codex 调 ast-grep run/scan
  → 输出 .ccteam/codex-review.md
```

Codex prompt 应强制四块：

```text
Goal: review current diff and use ast-grep for syntax-aware checks.
Context: .ccteam/spec.md, plan-eng.md, architecture.md, git diff.
Constraints: read-only; do not modify files; use ast-grep for structural claims.
Done when: output PASS / CONCERN / BLOCK with ast-grep evidence when applicable.
```

增强效果：

- Codex 不是只靠自然语言 review。
- 对“重复模式”“危险 API”“批量遗漏”的命中更稳定。
- 输出可以被 ccteam gate 解析。

### 5.2 Codex AGENTS.md + ast-grep

Codex 官方支持 `AGENTS.md`。ccteam 未来生成 Codex 项目手册时应加入：

```markdown
## Structural Search

When a task requires syntax-aware search or evidence-backed review,
prefer ast-grep before plain text grep.

Use plain rg for string search.
Use ast-grep for code shapes, API usage patterns, AST-level rewrites,
or project-specific rules under .ccteam/ast-grep/rules/.
```

这比临时 prompt 更稳定，符合 Fat Skills 文档中“把经验沉淀成工作约定”的方向。

### 5.3 Codex skill + ast-grep

Codex 官方支持 `.agents/skills`。ccteam 可以生成：

```text
.agents/skills/ast-grep-review/SKILL.md
.agents/skills/ast-grep-codemod/SKILL.md
```

skill 内容负责教 Codex：

- 如何把自然语言问题拆成 ast-grep rule。
- 如何先 dry-run 再 rewrite。
- 如何输出 PASS/CONCERN/BLOCK。
- 如何把 findings 写到 `.ccteam/ast-grep/`。

这与 `thin-harness-fat-skills` 文档完全一致：复杂工作流放在 Markdown skill，不写进 Rust。

---

## 6. 集成方式设计

### 6.1 M0：只作为 CLI 工具接入

最小集成：

```bash
ast-grep --version
ast-grep run -p '<pattern>' -l <lang> --json
ast-grep scan --rule <rule.yml> --json
```

ccteam 改动：

- 文档化推荐用法。
- 在 phase prompt / skill 中提示何时使用。
- 不改 schema。

适用：

- 让 Claude Code / Codex 通过 Bash 自行调用。
- 开发者手动验证收益。

### 6.2 M1：doctor + tool surface

新增：

```bash
ccteam doctor --ast-grep-surface
ccteam doctor --install-ast-grep-skill
ccteam doctor --install-ast-grep-mcp
```

`tool_surface.rs` 增加：

```rust
pub struct AstGrepSurfaceSnapshot {
    pub cli_available: bool,
    pub version: Option<String>,
    pub claude_skill_available: bool,
    pub codex_skill_available: bool,
    pub mcp_available_for_claude: bool,
    pub mcp_available_for_codex: bool,
}
```

不建议把它硬塞进现有 `ToolSurfaceSnapshot`，因为 ast-grep 横跨：

- CLI binary。
- Claude skill。
- Codex skill。
- Claude MCP。
- Codex MCP。

这些加载路径不同，需要独立 surface。

### 6.3 M2：phase YAML 声明结构化工具需求

当前 `tools_required` 只有：

```yaml
tools_required:
  subagents: []
  skills: []
  mcp: []
```

建议未来扩展：

```yaml
tools_required:
  binaries:
    - ast-grep
  skills:
    - ast-grep
  mcp:
    - ast-grep
```

或者更保守地新增 team-level：

```yaml
external_tools:
  binaries:
    - name: ast-grep
      command: ast-grep --version
      required_for: [plan-eng, implement, review]
```

架构取舍：

- `binaries` 进入 `tools_required`：统一但需要改 `interfaces.md`。
- `external_tools` 进 `team.yaml`：更适合 team pack，但 phase 级可见性弱。

推荐先做 `external_tools`，等多工具需求稳定后再推广到 phase schema。

### 6.4 M3：ast-grep rule pack

约定项目目录：

```text
.ccteam/ast-grep/
  sgconfig.yml
  rules/
    no-debug-log.yml
    no-unsafe-unwrap.yml
    no-direct-shell.yml
  findings.json
  report.md
```

team 可以声明：

```yaml
ast_grep:
  rule_dir: .ccteam/ast-grep/rules
  advisory_rules:
    - no-debug-log
  blocking_rules:
    - no-direct-shell
```

orchestrator 不解析 rule 语义，只运行 scan wrapper 并读取标准 summary。

### 6.5 M4：sub-skill / sidecar runner

参考 Codex integration 文档中的 `CodexExecRunner`，可以增加 `AstGrepRunner`：

```rust
pub struct AstGrepRunner {
    argv: Vec<String>,
}
```

触发方式：

```yaml
sub_skills:
  - skill: local:.ccteam/ast-grep/rules/no-direct-shell.yml
    trigger: phase_done
    runner: ast_grep_scan
    output_to: .ccteam/ast-grep/report.md
```

更好的方式是不要把 ast-grep 做成“LLM sub-skill”，而是做成 deterministic sidecar：

```text
phase_done
  → run ast-grep scan
  → write findings.json/report.md
  → Codex/Claude reviewer 读取 report
```

### 6.6 M5：ast-grep MCP

官方 `ast-grep-mcp` 是实验性 MCP server，适合让 Claude Code / Codex 迭代开发复杂 rule：

```text
Claude/Codex
  → ast-grep MCP dump AST / test rule / run search
  → refine rule
  → save rule under .ccteam/ast-grep/rules
```

适用：

- 复杂 rule 开发。
- 需要 AST debug 的 pattern。
- agent 自己写 rule，但先在 snippet 上反复测试。

不建议：

- 默认给每个项目都开启 MCP。
- 在无 sandbox 的环境里直接从远端 `uvx` 启动。
- 把 MCP 输出直接塞进主 session 大上下文。

---

## 7. 在 ccteam phase 中的落点

### 7.1 `plan-eng`

新增产物建议：

```text
.ccteam/ast-grep/context.md
```

内容：

- 本项目语言栈。
- 关键结构 pattern。
- 命中路径列表。
- 影响范围判断。
- 哪些 pattern 失败或不适用。

### 7.2 `implement`

使用 ast-grep 的场景：

- 找所有需要改的 API usage。
- 批量 rewrite。
- 验证“所有旧 pattern 已消失”。

原则：

- rewrite 前必须有 dry-run artifact。
- rewrite 后必须跑测试。
- 不把 rewrite 作为 review 替代。

### 7.3 `test-run`

使用 ast-grep 查测试覆盖形状：

- 是否新增了目标函数的测试。
- 是否测试文件里存在对应 fixture。
- 是否还有 skip/only。

注意：这只是结构检查，不能证明测试质量。

### 7.4 `fix`

fix loop 中可以让 agent：

```text
失败测试 → ast-grep 找相关调用形状 → 缩小改动范围 → 修复
```

这能减少“盲目全仓搜索 + 大范围修改”的风险。

### 7.5 `ship`

ship phase 可读取：

```text
.ccteam/ast-grep/report.md
.ccteam/codex-review.md
.ccteam/code-review.md
```

形成 gate summary：

```json
{
  "gates": [
    {"name": "tests", "verdict": "PASS"},
    {"name": "codex-review", "verdict": "PASS"},
    {"name": "ast-grep-blocking-rules", "verdict": "PASS"}
  ],
  "overall": "PASS"
}
```

---

## 8. 典型增强场景

### 8.1 API 迁移

问题：

```text
把旧的 foo.bar(x, y) 迁移成 foo.baz({x, y})
```

仅靠 LLM：

- 容易漏调用点。
- 容易改到注释或字符串。
- 大仓库上下文不够。

ast-grep 增强：

```bash
ast-grep -p '$OBJ.bar($X, $Y)' -l ts
```

然后 dry-run rewrite、测试、review。

增强明显。

### 8.2 安全 anti-pattern

问题：

```text
禁止直接拼 shell 命令或 SQL 字符串。
```

ast-grep 可以成为 blocking rule，但注意它只能覆盖语法形状，不能替代 taint analysis。

增强中到强，取决于 rule 质量。

### 8.3 React / UI 结构检查

问题：

```text
找所有 useEffect 里直接调用 fetch 的组件。
```

`rg fetch` 噪声高；LLM 全仓读成本高。ast-grep 可先缩小候选。

增强明显。

### 8.4 Rust 项目质量 gate

问题：

```text
新代码不允许在 public API 路径里 unwrap。
```

Clippy 未必表达项目语义；review 容易漏。ast-grep 可以成为 team-specific rule。

增强明显，但需要 rule 测试。

### 8.5 小型一次性功能

如果项目只有几个文件，需求是新增一个简单 CLI 参数，Claude/Codex 自带 `Read`、`rg`、测试已经够用。

增强不明显。

---

## 9. 风险与限制

| 风险 | 说明 | 缓解 |
|---|---|---|
| rule 幻觉 | LLM 写错 ast-grep rule，导致漏报或误报 | 使用 `ast-grep test` / MCP AST debug / 示例 snippet |
| 过度使用 | 简单文本搜索也强行 ast-grep | phase prompt 明确 `rg first for text` |
| rewrite 误伤 | 批量改写破坏行为 | dry-run、diff、测试、人工/agent review |
| 无类型语义 | AST 规则不懂跨文件类型和数据流 | 与 compiler/linter/CodeQL/Semgrep 分工 |
| MCP 供应链 | `ast-grep-mcp` 是实验性 server，安装方式可能引入风险 | 默认 CLI，MCP opt-in，doctor 显示来源 |
| 输出过大 | 大仓库 scan 输出可能撑爆上下文 | 写文件，摘要注入，分页/过滤 |
| 多语言差异 | tree-sitter grammar 对语言特性覆盖不同 | team/rule pack 标注支持语言和已测样例 |

---

## 10. 架构建议

### 10.1 是否引入

建议引入，理由：

- 与 ccteam Thin Harness 原则一致：确定性工具，不把智能写进 Rust。
- 与 Fat Skills 方向一致：LLM 学会何时写 rule，rule 本身可沉淀。
- 与 Codex sidecar 方向一致：review 输出能有工具证据。
- 与 ccteam 文件协议一致：findings/report 可落 `.ccteam/`。

### 10.2 引入优先级

优先级从高到低：

1. **CLI + skill 文档化**：低风险，马上可用。
2. **doctor surface**：让环境问题 fail loud。
3. **review/QA report artifact**：让 Codex/Claude review 证据化。
4. **rule pack + gate**：把经验转成可复用规则。
5. **MCP rule development**：用于复杂规则，不默认开启。
6. **codemod 自动化**：最后做，必须加 dry-run 和测试 gate。

### 10.3 是否带来明显增强

结论：**会，但不是全局 10x；是特定任务上的强增强。**

明显增强的地方：

- 大仓库影响范围分析。
- API migration / codemod。
- review 中寻找结构性遗漏。
- 项目特定规则和 retro 经验固化。
- Claude/Codex 的 evidence-backed review。

不明显的地方：

- 小改动。
- 纯文本查找。
- 类型/数据流/运行时行为。
- 已经有成熟 linter 覆盖的问题。

架构师视角的判断：ast-grep 应作为 **ccteam 的 structural evidence layer**，而不是“又一个 agent”。它让 Claude Code 和 Codex 的判断更可验证，让 ccteam 的 review gate 更可重复。

---

## 11. 推荐路线图

### M0：研究文档与手动使用

- 新增本文档。
- 在 phase author guide 中加入 ast-grep 使用边界。
- 在 Codex/Claude prompt 示例中加入 “structural search 用 ast-grep，文本搜索用 rg”。

### M1：环境检查

- `ccteam doctor --ast-grep-surface`
- 检查 `ast-grep --version`
- 检查 Claude skill / Codex skill / MCP 配置是否存在。

### M2：structural context pack

- `plan-eng` 可选生成 `.ccteam/ast-grep/context.md`
- Codex review prompt 读取该 context。
- Web UI 展示 structural findings 摘要。

### M3：rule pack 与 advisory report

- `.ccteam/ast-grep/rules/`
- `.ccteam/ast-grep/report.md`
- phase_done 后 advisory scan。

### M4：blocking gate

- team.yaml 声明 blocking rules。
- ship phase 读取 gate summary。
- BLOCK 进入 fix 或 escalation。

### M5：codemod pipeline

- dry-run artifact。
- rewrite plan。
- apply rewrite。
- test。
- Codex/Claude review。

---

## 12. 对现有文档和代码的影响

| 区域 | 建议改动 |
|---|---|
| `docs/claude-code-tool-surface.md` | 增加 ast-grep 与 `rg` / MCP / skill 的边界 |
| `docs/interfaces.md` | 若新增 `tools_required.binaries` 或 `ast_grep` schema，必须同步 |
| `docs/tech-design.md` | 如果 ast-grep gate 进入主流程，补充 structural evidence layer |
| `crates/ccteam-core/src/tool_surface.rs` | 增加 AstGrepSurfaceSnapshot 或 external binary snapshot |
| `crates/ccteam-core/src/subskill.rs` | 可选增加 deterministic sidecar runner |
| `teams/dev/phases/02-plan-eng.md` | 增加 structural context pack 建议 |
| `teams/dev/phases/03-implement.md` | codemod dry-run 规则 |
| `teams/dev/phases/09-ship.md` | 读取 ast-grep report/gate summary |

---

## 13. 参考链接

- ast-grep GitHub: <https://github.com/ast-grep/ast-grep>
- ast-grep docs: <https://ast-grep.github.io/>
- CLI reference: <https://ast-grep.github.io/reference/cli.html>
- YAML rule reference: <https://ast-grep.github.io/reference/yaml.html>
- Supported languages: <https://ast-grep.github.io/reference/languages.html>
- Using ast-grep with AI tools: <https://ast-grep.github.io/advanced/prompting.html>
- ast-grep MCP: <https://github.com/ast-grep/ast-grep-mcp>
- Claude Code MCP docs: <https://code.claude.com/docs/en/mcp>
- Codex MCP docs: <https://developers.openai.com/codex/mcp>
- Codex Skills docs: <https://developers.openai.com/codex/skills>

