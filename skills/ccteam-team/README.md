# ccteam-team — skill 入口速查

primary path:在用户当前 Claude session 里起一个 Anthropic Agent Team。
**零 ccteam workflow.yaml 依赖** — 任意 git repo 都跑得起。

> 完整协议见 `SKILL.md`。本 README 给 `ccteam-creator` skill / docs 等交叉引用用。

## 入口语法

```
/ccteam-team <task>                              # auto N + auto roles
/ccteam-team N "<task>"                          # N teammates, you decide roles
/ccteam-team N:role "<task>"                     # N teammates, all role=<role>
/ccteam-team auto "<task>"                       # explicit alias of form 1
```

## Trigger examples

| 输入 | 效果 |
|---|---|
| `/ccteam-team "build a Next.js blog with researcher / frontend / reviewer"` | auto 3-role team |
| `/ccteam-team 3 "fix TS errors across src/"` | 3 teammates, mixed |
| `/ccteam-team 3:debugger "fix build errors in src/auth/"` | 3 个 debugger 并行 |
| `/ccteam-team 5:reviewer "review the new API design"` | 5 个 reviewer debate |
| `/ccteam-team auto "investigate why integration tests flake"` | auto N + roles |

## Plan-first protocol(红线)

skill 强制 plan-first:第一条 assistant message 是 `TEAM PLAN ===` 格式,STOP,等用户
`go` / `yes` / `approve` 才调 `TeamCreate` + `Task` spawn teammates。详 `SKILL.md` §3-4。

## 安装

```bash
ccteam doctor --install-skill all   # 装 ccteam-team / ccteam-creator / ccteam-control
```

或单装本 skill:

```bash
ccteam doctor --install-skill ccteam-team
```

## 跟 sibling skill 关系

| skill | 用途 |
|---|---|
| **`ccteam-team`** (本 skill) | 在当前 session 内起 agent team |
| `ccteam-creator` | 创 ccteam project / workflow.yaml / agent.md |
| `ccteam-control` | 管 ccteam daemon + MCP 工具调用 |
