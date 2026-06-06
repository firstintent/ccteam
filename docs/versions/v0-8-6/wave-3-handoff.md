# Wave 3 Handoff —— 删除/停止(project rm / project stop)

> v0.8.6 W3,stacked 在 v086-w2 上。一次提交。baseline 1865 → **1881/0**(+16 rm/stop/purge 测试,additive)。clippy 0,fmt 干净。**附 purge-preservation 审计**(verify agent 实跑 CLI binary 证明用户文件不被删)。

## Decided(PRD Item 3)
- **可复用引擎 = 既有 `run_remove(paths, slug, RemoveOptions) -> RemoveReport`**(commands.rs)。`RemoveReport.steps` 即 dry-run 计划;`opts.dry_run` 只渲染不动手。**未**另造 `remove_project`/`RemovePlan`(pre-v1.0 edit-existing,避免冗余 churn)。flat `ccteam remove` + 新 `project rm` 都走它(flat 由 W4 删)。
- **`project` clap 组**(main.rs):`Rm { slug, --purge, --dry-run, --force }` + `Stop { slug }`,**与 flat 命令并存**(W4 才做 flat→grouped 全量重组)。
- **`--purge` 范围 = ccteam footprint(W2 布局对齐)**:删 `<proj>/.ccteam/`(=state.json+workflow.yaml)、`<proj>/.claude/agents/cto.md`(只删种入的 persona,不删 agents/ 目录)、`<proj>/.claude/settings.local.json` 里 ccteam 的 hook 段(新增 `ccteam_core::remove_chat_hooks` 外科剔除,塌成 `{}` 才删文件)、config.yaml 注册项、`~/.ccteam/{progress,imd/registry}/<slug>`。**保留**:用户 work-role(`.claude/agents/*.md` 非 cto)、`CLAUDE.md`/`AGENTS.md`、`.env`、用户 `.claude/settings.json`、业务代码。
- **`project stop <slug>`**:停项目所有 role-session = daemon-independent 枚举 `ccteam-chat-<slug>-*` tmux session(`tmux_ops::list_sessions` + `parse_chat_session_name`,**dash-aware**:解析后 slug 全等才匹配,不误伤 sibling)→ 逐个 `kill`(幂等,resumable;explicit-command 例外,不违「永不主动 kill」);停 0 个也算成功。
- **`project rm` = stop-then-delete**:先停活动 chat session 再 deregister/purge;`--force` 跳过 refusal-gate 确认;`--dry-run` 列「would stop …」+「would remove …」不动手。

## Rejected
- 新造 `remove_project`/`RemovePlan` 平行类型:既有 run_remove 已是引擎,edit-existing。

## Risks
- `project stop` 走 tmux 枚举(非 daemon 内存态):CLI 与 daemon 异进程,这是正确的 process-independent 做法;daemon 下次交互按 W1 `--resume` 重建,符合 resumable 语义。
- 单 session 删(`session rm`)未做 —— 非本版必需,推后(PRD D3.2)。

## Files
- ccteam-cli:commands.rs(run_remove 引擎 + run_project_stop + stop_project_chat_sessions + purge_project_managed_paths W2 对齐)、main.rs(`project` 组)、tests/remove_test.rs(t01–t18,20 测试)。
- ccteam-core:tool_surface.rs(新 `remove_chat_hooks` + 7 单测)、lib.rs(导出)。

## Remaining(后续 wave)
- **W4**:flat→grouped CLI 全量重组(删 flat `remove`/别名;`project` 组补 ls/show/new;`session` 组);`config` 交互菜单;skill 转化(creator→cto+init、im-setup→config)。
- 单 session 粒度删除(`session rm <slug> <role>`)推后。
