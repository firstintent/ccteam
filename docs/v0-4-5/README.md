# V0.4.5 — hotfix: watcher 路径 + phantom agent_spawn cleanup

> 2 finding hotfix。Ship 后归档。

## 2 个 finding

### F78 — `artifact_watcher` 项目相对路径修复 + progress.jsonl 参数

V0.4.0 F64 `ArtifactWatcher::new` 接受 `Vec<(role, watch_path)>` + project_dir,内部应该把 relative watch_path resolve 成 absolute path 再传给 inotify。但实现里**只在 caller(`orchestrator.rs::run_project`)做了 resolve**,而某些 spawn 路径(reload / 手动 trigger)直接传 relative path 给 watcher → inotify 注册了相对当前工作目录的路径(daemon cwd 不是项目根)→ 实际监听了**错误的目录**,artifact_received 事件永不触发。

**修**:
- `ArtifactWatcher::new` 内部 resolve 所有 watch_path 为 absolute(`project_dir.join(relative)`)
- `progress.jsonl` 路径不再传 project_dir,改传 `progress_path` 全路径(参数语义对齐 spec)

### F80 — phantom `agent_spawn` cleanup

V0.4.4 后 host 累积 N 个项目,daemon 重启时 progress.jsonl 显示 running_count 高(7+ 之类)但实际无 claude 进程 — 都是上次 daemon SIGKILL 时 in-flight 的 agent_spawn 没匹配 agent_done,事件流"开始没结束"挂着。

**修**(orchestrator 启动钩子 + 新 helper):
- 新 `claude_job::probe_job(job_id) -> JobLiveness::Terminal { status, cost_usd }` — 读 `~/.claude/jobs/<id>/state.json` 判活
- 新 `progress::current_agent_sessions_with_liveness<F>(events, probe)` — 把 open `agent_spawn` 跟 liveness 关联
- orchestrator 启动 + 周期扫描:发现 stale spawn(probe 返 Terminal/Missing)→ 写 synthetic `agent_done` 收尾
- SPA WorkflowView 加 `.agent-active-dot` pulse(`running_count > 0` 才亮)— 用户能直观看到"真有在跑"

## 触发

V0.4.4 落地后 host 多次重启 daemon,SPA 显示 "running=7 cost=$0" 但 ps 无 claude — F80 unblock 这种 phantom 数字。

## 与 V0.4.4 / V0.4.6 的关系

- V0.4.4(F77)修了 slug → path 解析,F78 是同一类(hardcoded 假设)的另一个 bug
- V0.4.6(F86)graceful shutdown 是 F80 的**根因修复** — daemon 不再 SIGKILL 自己,phantom spawn 不再产生(F80 cleanup 退化成 startup 防御)

## 详情

- F78:1 个 commit,修 `artifact_watcher.rs` + `orchestrator.rs::run_project`
- F80:18 个新测试 + 4 个新源文件 helper(`claude_job::probe_job` / `current_agent_sessions_with_liveness` / 等)

代码改动较大但分散,无独立 PRD。Commit `28f74d5` (F78) + `deba0a4` (F80)。
