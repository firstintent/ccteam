# agents/ — ccteam 默认 agent scaffold templates

`ccteam init` 生成新项目的 `<project>/.claude/agents/*.md` 时,**这些文件就是源模板**。Rust 端通过 `include_str!` 把内容嵌入 binary。改 `.md` 文件 → `cargo build` 重编 → 下次 `ccteam init` 拿到新模板。

## 当前默认 agents

| 文件 | role | 触发方式 | 默认工具集 |
|---|---|---|---|
| `explorer.md` | explorer | `manual` | Read / Glob / Grep / Bash(read-only) / WebFetch |

`ccteam init` 同时写一份默认 `workflow.yaml`(在 `<project>/.ccteam/workflow.yaml`),声明 `explorer` role + `trigger: manual`。

## 规范(Anthropic agent frontmatter)

每个 agent .md 必须有 YAML frontmatter:

```markdown
---
name: <role>                    # 必填,等于文件名去掉 .md
description: |                  # 必填,1-N 行,what this agent does + when to trigger
  ...
tools: Read, Glob, Bash, ...    # 选填,逗号分隔工具列表;省略 = 继承全 tool surface
model: claude-sonnet-4-6[1m]    # 选填,默认 inherit
color: blue                     # 选填,UI 视觉标识(blue/green/red/yellow/pink/...)
---
<system prompt body — what the agent does, boundaries, outputs, escalation rules>
```

详见 `docs/claude-code-tool-surface.md` + `docs/orchestration-patterns.md §一` 的"按上下文拆"哲学。

## 加 agent 步骤

1. 在本目录加 `<role>.md` 文件(写 frontmatter + system prompt)
2. 在 `crates/ccteam-cli/src/commands.rs::DEFAULT_AGENT_SCAFFOLDS` 加 `include_str!("../../../agents/<role>.md")` 条目
3. 如果该 agent 是默认 workflow.yaml 应该带的,改 `DEFAULT_WORKFLOW_YAML` 加 role 声明
4. `cargo build --workspace` 重编;`cargo test --workspace` 跑过
5. 用户下次 `ccteam init` 新项目时就会拿到新的 scaffold

## 命名约定

- 文件名 = role 名 = workflow.yaml `agents:` 表里的 key,小写 + dash(`code-reviewer`, `db-migrator`, etc.)
- description 直接面向"为什么要 spawn 这个 agent",不重复 role 名
- description 第一句必须是"做什么 + 何时 trigger"(orchestrator 加新 finding 时会 cite description)

## 跟其他 agent 模板的关系

- ccteam **team templates**(`crates/ccteam-core/src/templates/teams/*/*.md`)是更高层的 team-wide scaffold(完整 workflow + N agents),给 `ccteam doctor --install-team` 用
- 本目录的 `agents/*.md` 是**最小起步集**,给 `ccteam init` 默认 workflow 用
- meta-agent 的 prompt(`crates/ccteam-core/src/templates/meta_agent_role.md`)是独立的 — 那是 ccteam-managed singleton,不走 init 流
