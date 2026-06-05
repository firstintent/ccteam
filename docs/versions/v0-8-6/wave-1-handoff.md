# Wave 1 Handoff —— IM session = role(keystone）

> v0.8.6 W1。worktree `v086-w1`(从 origin/dev 切)→ PR 回 dev。baseline **1912 → 1913/0**(+1 新测试),clippy 0(`-D warnings`),`cargo fmt --all` 干净。

## Smoke gate(头号风险,已实证过)
真实 `claude` 在**我们的 tmux send-keys 路径**跑通(临时项目 + `cto.md` + 假 hook 观测哪些事件触发):
- `claude --agent cto --name <sid>` 交互可答;`--resume <name>`(按 `--name` 别名)恢复**同一** session(同 session_id,上下文带回 —— 问「刚才的算术结果」答「Four.」)。
- `--agent` 下 hooks **全触发**:SessionStart / UserPromptSubmit / **Stop**(→ `chat_turn_completed`,turn 完成检测的命根)/ Pre+PostToolUse(真跑了 Bash 工具)。
- persona 经 `--agent` 从 `.claude/agents/cto.md` 加载(transcript 出现约定 token);change-persona 在 **fresh start** 生效(新 sid 拿到新 token → 无陈旧 agent 缓存)。
- 结论:**session=role 模型成立**,可放心建在它之上。脚手架在 `/tmp/ccteam-w1-smoke/`(临时、已弃)。

## Decided
- **spawn argv**:`spec_for_new` / `spec_for_resume` 插入 `--agent <role>`,定序 `[claude, --agent, <role>, --dangerously-skip-permissions, --name|--resume, <sid>]`(smoke 实证形,其余元素不动序)。`spec_for_fresh` 委托 `spec_for_new`,自动继承。
- **cto.md 单一源**:内容在 `ccteam-core/src/templates/cto_role.md`,导出 `ccteam_core::CTO_ROLE_MD`;CLI scaffold(`DEFAULT_AGENT_SCAFFOLDS`)+ core `bootstrap_project_at_dir`(IM `/newproject` / gateway create_project)**都**种它 —— 无论哪条建项目路径,默认 `--agent cto` 都有文件可加载。
- **默认 role cto**:gateway `/new` 默认 + `ensure_current_session` 兜底,`assistant`→`cto`。
- **`/role <role>`**:**原地切换当前 session**(新 `switch_current_role`)—— abort 旧 event pump + `close_thread` 旧 pane,以新 `--agent <role>` `start_thread`,**保持同一 gateway sid**(`/use <sid>` 不失效);无活动 session → 中文报错;同 role → no-op(不白扔 live context);保留原 vendor。`/role` 进 `GATEWAY_COMMANDS`(`in_menu:false`,带必填 arg)→ `/help` 自动列出。

## Rejected
- `/role` 复用 `start_session`:它按 `(project, role)` 去重 + 会发**新 sid**,破坏「同 sid」验收 → 故新写 in-place swap。
- 删 repo 根 `agents/explorer.md` **文件**:其他路径仍引用,只改了 init **默认种**什么(explorer→cto),文件留着。
- `--agent` 缺 role.md 时降级为无-agent session:本版不做(init 必种 cto.md;pre-v1.0 重装策略兜底)。见 Risks。

## Risks(交给 review / 后续 wave)
- **SubagentStop**:`--agent` 顶层 turn 有时**也**触发 `SubagentStop`(session 被建模为 implicit-main 的 subagent);`Stop` 始终触发,故 turn 完成可靠。已核实**不会双发 IM 回复** —— 回复只走 transcript-content track(`ItemCompleted{AgentMessage}`),hook track(stop/subagent-stop)只写 `progress.jsonl`,两轨解耦(代码无 `chat_subagent_completed` 常量,subagent-stop 仅是 progress action)。W6 文档值得记一笔。
- **change-persona on `--resume`**:resume 会沿用**历史里展现的** persona(in-context 模仿,非缓存);只有 fresh start 才换。`/role` 本就 fresh-spawn,无影响;W6 usage 注明。
- **缺 cto.md 的旧项目**:W1 前 init 的项目(有 explorer.md、无 cto.md)在默认 cto 下 `--agent cto` 会失败 →(且 resume-fail fallback 也带 --agent → 同错)。pre-v1.0「清旧数据 + 重 init」策略接受;若要稳,后续可给 `--agent` 加「role.md 不存在则降级」—— 本版未做。
- **codex_exec_wave3_test 偶发 timeout**:与 W1 无关的既有 flake(隔离跑 + 全量重跑均过,1913/0);非回归。

## Files
| 文件 | 改动 |
|---|---|
| `crates/ccteam-harness/src/execution/claude_tui.rs` | `spec_for_new`/`spec_for_resume` 加 `--agent <role>` + doc-comment |
| `crates/ccteam-harness/tests/claude_tui_resume_test.rs` | 5 个 argv 测试加 `--agent <role>` 断言(new/resume/fallback/collision/restart）|
| `crates/ccteam-core/src/templates/cto_role.md`(新)| cto persona(默认管家 role,3 职责）|
| `crates/ccteam-core/src/templates/mod.rs` + `lib.rs` | `CTO_ROLE_MD` const + 导出 |
| `crates/ccteam-core/src/projects.rs` | `bootstrap_project_at_dir` 种 `.claude/agents/cto.md`(absent-only）|
| `crates/ccteam-cli/src/commands.rs` | `DEFAULT_AGENT_SCAFFOLDS`→cto.md(core 单一源)+ 4 处 init 测试 explorer→cto |
| `crates/ccteam-im/src/gateway.rs` | 默认 role cto + `/role` + `switch_current_role` + `GATEWAY_COMMANDS` + 新测试 |

## Remaining(本 wave 不做,后续）
- 老项目缺 cto.md 的降级(可选,见 Risks)。
- Codex role 对齐推后:本版只保证 Codex 读 `AGENTS.md`,role 绑定推后(PRD DA.5)。
- work-role import/picker UI 推后下版(PRD DA.2b);本版选 role = 手动丢 `.md` + `/role`。
- W6 文档:把 SubagentStop / resume-persona 两条 finding 写进 tech-design / usage;CLAUDE.md §〇/§一/§三 红线随 W6 重写(本 wave 未碰 stale 文档)。
