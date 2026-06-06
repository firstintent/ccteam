# Wave 2 Handoff —— 目录/模板/工具清理(deletion wave）

> v0.8.6 W2,stacked 在 v086-w1 上(origin/dev 尚无 W1)。两次提交:**W2a** `e1a4e20`(deletions)+ **W2b**(init/template refactor)。baseline 1913 → **1865/0**(deletion wave,降幅已逐条对账)。clippy 0(`-D warnings`),`cargo fmt --all` 干净。流程:read-only recon(6 agent)→ adversarial 实证 → 并行 cut(disjoint files)→ verify+fix gate。

## Smoke(本 wave 新验)
hooks 定义在 **`.claude/settings.local.json`**(无 settings.json)时,真实 `claude --agent cto` 下 SessionStart/UserPromptSubmit/**Stop** 全触发 —— D2.5 迁移安全,IM turn 检测不受影响。

## Decided
- **itemF MCP 瘦身**:删 7 个 F65 `workflow_*` orchestration 工具 + `chat_reset`。工具数 **28→20**(workflow 8 / chat 6 / admin 3 / screenshot 1 / advise 2);同步 mcp_serve / mcp_tool_groups / `doctor --verify-mcp` / e2e 计数测试 + plugin manifests。
- **itemE skill 删除**:删 `ccteam` / `ccteam-advise` / `ccteam-team` / `ccteam-scan`(目录 + skill.rs install fn/const/test + manifests)。**保留** control / creator / im-setup(W4 转化),以及 **live 的 advise.rs 后端 + advise_vote/parallel MCP 工具**(skill 只是 UI 入口,后端不动)。
- **§6 死文件**:停写 `.ccteam/ready`;删悬空 webhook 路由(整文件 + ccteam show URL + paths.rs orphan project_ready_in/webhooks_dir/webhook_token)。
- **item1 home 目录**:init 停建 `queue` / `memory` / `log`;新增 `ccteam_core::canonical_home_dirs()` 单一布局 manifest;`doctor` 加 home-layout drift 报告。
- **item2 init/模板**:① 不再生成项目 CLAUDE.md/AGENTS.md(项目知识=vendor 原生)——删 render_project_claude_md + generic_claude_md_template + resolve_project_team_spec;② `.ccteam/` 只留 state.json + workflow.yaml(删 spec.md 写入 + scaffold_v81 的 .ccteam/agents 中立拷贝/skills/.gitkeep;**W1 的 .claude/agents/cto.md 保留**);③ ccteam 托管设置写 `.claude/settings.local.json`(hook + base settings;不碰用户 settings.json);④ slug 撞名数字累加 demo/demo2/demo3(弃 -{4hex},public 签名不变)。

## Rejected / Adjusted(ground-truth 修正)
- **flex TYPES 未删(只删 CLI)**:recon 说 flex「无 live consumer」**是错的** —— `TeamKind::Flex` / `ProjectState.sessions` / `SessionRecord` 织进 ~30 处**已编译**代码(core 的 progress_jsonl_for_context + queries + screenshot、ccteam-web routes、hooks)。本 wave 删了 `ccteam session` CLI 子命令族 + flex 测试;**类型保留**。full 类型 EOL **推到 W5**(W5 重写 ccteam-web,自然重做那些 state.sessions consumer,避免双重 churn)。cut agent 按 STOP 规则正确拒切。
- **D1.4(orchestrator.pid→daemon.pid + heartbeat 删)推后**:heartbeat 有 live writer(daemon.rs),低优,避免动 daemon pid;留后续。
- **D2.4 模板集中(inline const→include_str!)推后**:可读性 nice-to-have,非功能;W1 已把 cto.md 移入 core templates,其余 inline const(DEFAULT_WORKFLOW_YAML 等)留 W4/chore。
- **doctor 的 legacy-hook scrub + tool_surface 仍按文件名碰 settings.json**:**有意** —— 这是把旧 ccteam hook 从用户 settings.json 里**清出去**的一次性迁移(path-param fn),与 D2.5「不脏用户 settings.json going-forward」一致。

## Risks
- flex 类型残留到 W5:中间 wave(W3 rm/stop、W4 CLI 分组)不依赖其删除,安全;W5 web 重写时一并 EOL。
- recon「safe」判定对跨切关注点不可全信 —— 已靠 cut agent 编译级 STOP + verify gate 兜住(本 wave 即抓到 flex 误判)。后续删除类继续 STOP-and-report。

## Files(crate 粒度)
- ccteam-cli:commands.rs / main.rs / mcp_serve.rs / mcp_tool_groups.rs / mcp_chat_tools.rs(+删 mcp_workflow_tools.rs)/ 多个 tests。
- ccteam-core:skill.rs / lib.rs / paths.rs / projects.rs / meta_agent.rs / team.rs(doc)/ templates/mod.rs / tool_surface.rs。
- ccteam-harness:execution/claude_tui.rs + claude_tui_test.rs(settings.local)。
- ccteam-hooks:load_context.rs(+ hooks_test.rs)。
- ccteam-web:routes/{mod.rs, 删 webhook.rs}/ lib.rs(+删 webhook_test.rs)。
- skills/:删 4 个目录;`.claude-plugin/plugin.json` + `.codex-plugin/plugin.json` 同步(剩 control/creator/im-setup)。

## Remaining(后续 wave)
- **W5**:flex 类型 full EOL(随 web 重写);深砍 workflow 查看/控制类 MCP(API 落地后)。
- 后续/chore:D1.4 pid-rename+heartbeat、D2.4 模板集中、doctor `--gc-home`。
- **CLAUDE.md / tech-design / usage 仍 stale**(skills 表、MCP 计数、flex、settings.json)→ **W6 全量重写**(本 wave 未碰 tier-1 doc,按计划)。
