# V0.4.6 — dev plan

实现顺序、文件改动、迁移策略。详 PRD 见 `prd.md`。

---

## 阶段 1:F82 workflow.yaml `enabled` + 热加载(底层)

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs` | `WorkflowSpec` 加 `pub enabled: bool` `#[serde(default = "default_true")]`;`default_true() -> bool { true }` |
| `crates/ccteam-core/src/orchestrator.rs` | event_loop 加 cancellation token(`tokio::sync::watch` 或 `mpsc::oneshot`);加 `unroster_project(slug)` + `reload_project(slug)` |
| `crates/ccteam-core/src/orchestrator.rs::run_project` | 选 spec 时 check `spec.enabled`,disabled 直接 return + 写 `workflow_done reason="disabled"` |
| `crates/ccteam-core/src/orchestrator.rs::spawn_new_rostered_projects` | 加 workflow.yaml 文件 watcher(新 helper `WorkflowFileWatcher`,逻辑独立于 artifact_watcher) |
| 新文件 `crates/ccteam-core/src/workflow_watcher.rs` | inotify on workflow.yaml(每个 rostered 项目一个),fire event on Modify;debounced 1s |
| `crates/ccteam-core/src/lib.rs` | re-export 新 API |
| `crates/ccteam-core/tests/workflow_enabled_test.rs` | 新 5 个测试 |

### 关键设计

**event_loop 终止机制**:目前 `watcher_handle.abort()` 是硬中断。改成:

```rust
let (cancel_tx, cancel_rx) = mpsc::channel(1);
let task_handle = tokio::spawn(async move {
    self.run_project_with_cancel(slug, cancel_rx).await
});
self.cancel_handles.lock().insert(slug, cancel_tx);
```

`run_project_with_cancel` 内部:
```rust
tokio::select! {
    _ = cancel_rx.recv() => {
        progress::append_event(&progress_path, &json!({
            "event": "workflow_done",
            "reason": "disabled" | "removed" | "reloaded",
            "slug": slug,
        }))?;
        return Ok(());
    }
    res = self.event_loop(...) => res,
}
```

**workflow.yaml 监听**:
- `WorkflowFileWatcher::new()` 接受 `Vec<(slug, project_dir)>`,装 inotify watch on `<project_dir>/.ccteam/workflow.yaml`(F83 后)或 root(fallback)
- emit `WorkflowFileEvent { slug, kind: Modified | Removed }` on debounced changes
- orchestrator 主循环加 select arm:收到 event → `reload_project(slug)`

**热 reload 语义**:
1. 加载新 spec(失败 → WARN + 保留老 loop)
2. 比较 `enabled` / `agents`(签名 hash):
   - 都不变 → no-op
   - 只 `enabled: false` → cancel 老 loop,写 workflow_done reason="disabled"
   - agents 变了 → cancel 老 loop + spawn 新 loop(`spawned: HashSet` remove,让 rescan 重新 add)

### 测试矩阵
- `t01_enabled_false_blocks_initial_spawn` — workflow.yaml `enabled: false` → 项目不进 roster
- `t02_enabled_true_after_false_starts` — 启动时 enabled=false,改成 true → 5s 内 loop 起来
- `t03_disable_running_project_clean_exit` — 改 enabled=false → workflow_done 事件 + JoinSet drop
- `t04_trigger_change_reload` — `watch:.ccteam/A/` → `watch:.ccteam/B/`,5s 内新 inotify register,A 上 noop
- `t05_yaml_syntax_error_fail_safe` — workflow.yaml 改成 invalid yaml → WARN log,老 loop 不动

---

## 阶段 2:F81 `ccteam remove <slug>`(用 F82)

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/main.rs` | 加 `Command::Remove { slug, purge, dry_run }` enum 分支 |
| `crates/ccteam-cli/src/commands.rs` | 实现 `run_remove(paths, slug, opts) -> Result<RemoveReport>`,守红线 + dry-run + purge 分支 |
| `crates/ccteam-core/src/config.rs` | 已有 `remove_project()` 复用 |
| `crates/ccteam-core/src/projects.rs` 新增 helper | `pub fn refuses_active_session(slug, project_dir) -> Result<()>` — tmux + claude bg + progress.jsonl 三检查 |
| `crates/ccteam-cli/src/mcp_serve.rs` | 加 `tool_remove` 给 MCP 调用 |
| `crates/ccteam-cli/tests/remove_test.rs` | 6 个测试 |

### 守红线实现(`refuses_active_session`)

```rust
pub fn refuses_active_session(slug: &str, project_dir: &Path) -> Result<()> {
    // 1. tmux session check
    if tmux_session_exists(&session_name_for_slug(slug))? {
        return Err(anyhow!("refusing — tmux session ccteam-{slug} 还活;先 tmux kill-session -t ccteam-{slug}"));
    }
    // 2. claude bg job check
    for entry in glob("~/.claude/jobs/*/state.json")? {
        let s = parse_cc_state_json(&entry)?;
        if s.cwd == project_dir && s.state == "working" && s.first_terminal_at.is_none() {
            return Err(anyhow!("refusing — claude bg job {} 还活;查 ~/.claude/jobs/{}/state.json", s.daemon_short, s.daemon_short));
        }
    }
    // 3. open agent_spawn in progress.jsonl
    let summary = workflow_summary(slug)?;
    let running: u32 = summary.agents.iter().map(|a| a.running_count).sum();
    if running > 0 {
        return Err(anyhow!("refusing — progress.jsonl 显示还有 {running} 个 running session;先看 `ccteam show {slug}` 决定怎么收尾"));
    }
    Ok(())
}
```

### CLI 签名

```
ccteam remove <slug>                              # 守红线 + 删 config + 不 purge
ccteam remove <slug> --purge                      # 同上 + rm -rf .ccteam .claude/agents workflow.yaml
ccteam remove <slug> --dry-run                    # 不动文件,只打印
ccteam remove <slug> --force                      # 跳过守红线(用户主动)
```

### 测试矩阵
- `t01_remove_dry_run_prints_only` — no fs change
- `t02_remove_basic_drops_config_entry` — config.yaml::projects[] 少一条
- `t03_purge_clears_ccteam_dir` — `.ccteam/` 不再存在,业务代码完好
- `t04_refuses_with_active_tmux` — 守红线生效
- `t05_refuses_with_running_claude_bg` — 同
- `t06_force_overrides_refusal` — `--force` 绕过

---

## 阶段 3:F83 workflow.yaml 移到 `.ccteam/`

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs::load_for_project` | 优先 `<project>/.ccteam/workflow.yaml`,fallback `<project>/workflow.yaml` |
| `crates/ccteam-cli/src/commands.rs::run_init` | 新建项目写到 `.ccteam/workflow.yaml`(root 不写) |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | 加 `--migrate-workflow-to-ccteam-dir [--apply]` 子选项 |
| `skills/ccteam-creator/SKILL.md` | 文档更新路径,提到旧位置 fallback |
| `docs/interfaces.md` §6 | 同步路径声明 |
| F82 的 `workflow_watcher.rs` | watch path 用新位置 |

### migration helper(`crates/ccteam-core/src/migration.rs` 加 entry)

```rust
pub fn migrate_workflow_to_ccteam_dir(
    paths: &CcteamPaths,
    dry_run: bool,
) -> Result<Vec<WorkflowMigrationReport>>
```

对 config.yaml::projects[] 每个项目:
- root 有 workflow.yaml + `.ccteam/workflow.yaml` 缺 → mv root → `.ccteam/`
- root 没 + `.ccteam/` 已有 → no-op
- root 有 + `.ccteam/` 也有 → fail-safe report,**不动**(防止覆盖),WARN 用户
- root 没 + `.ccteam/` 也没 → no-op(无 workflow 项目,V0.3 legacy)

### 测试矩阵
- `t01_load_for_project_prefers_ccteam_dir` — 两个位置都有 → 取 `.ccteam/`
- `t02_load_for_project_falls_back_to_root` — 只 root 有 → 加载 root
- `t03_init_writes_to_ccteam_dir` — 新建项目 workflow 在 `.ccteam/workflow.yaml`
- `t04_migration_moves_root_to_ccteam_dir` — `--apply` 后 root 没了
- `t05_migration_refuses_on_both_present` — 两边都有,不动

---

## 整体:CLAUDE.md baseline 更新

- §一 表格:`workspace version` 改为 `0.4.6`
- `当前 next` 移走 V0.4.6 完成的条目;留下 `~/.claude/jobs/` GC、真 cron、web token clipboard 等
- 测试 baseline 加 16(F82=5 + F81=6 + F83=5)→ 大概 ~741/0

## 不破红线 checklist

- [ ] `ccteam remove --force` 之外路径,绝不动用户业务代码(项目根除 `.ccteam/` + `.claude/agents/` + workflow.yaml 之外的内容)
- [ ] 守红线"永不主动 kill 长 session" — remove 检测 tmux + claude bg + running spawn,refusal 上手前不动
- [ ] workflow 热 reload 用 cancellation token,**绝不**用 `watcher_handle.abort()` 强杀(那会丢 in-flight 进度)
- [ ] `.env` 永远不删 — 密钥归用户

## 升级路径(用户视角)

V0.4.5 → V0.4.6 一次性迁移(`ccteam doctor` 提示):
1. `ccteam doctor --migrate-workflow-to-ccteam-dir --apply` — 把 N 个项目的 workflow.yaml 从 root 移到 `.ccteam/`
2. (无需 daemon 重启 — F82 热 reload 直接识别新位置)
3. `ccteam doctor` 走完后告知:删项目用 `ccteam remove <slug>`;暂停用 `enabled: false`

V0.4.5 用户没用新功能 → 0 break。
