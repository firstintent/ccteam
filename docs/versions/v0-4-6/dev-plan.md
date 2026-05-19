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

---

## 阶段 4:F91 cost SoT 收敛(F84 + F90 的前置)

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/main.rs` | 删 `Hook::CostAccumulate` enum branch + run_hook dispatch |
| `crates/ccteam-hooks/src/lib.rs` | 删 `pub fn cost_accumulate(...)` 及测试 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | settings.json 模板里删 `cost-accumulate` hook 生成 + 加 `--update-hooks` 清现有项目 |
| `crates/ccteam-core/src/state.rs` | `pub cost_used_usd: f64` 加 `#[serde(default)]` + `#[deprecated(note = "Use workflow_summary.cost_24h_usd, removed in V0.5")]` |
| `crates/ccteam-core/src/queries.rs` | `workflow_summary` 返回新 `CostSummary { cost_24h_usd, cost_active_usd, cost_total_usd }` |
| `crates/ccteam-core/src/queries.rs` 新增 | `pub fn cost_summary(slug, progress_path) -> CostSummary` — read progress + active state.json |
| `crates/ccteam-cli/src/commands.rs::run_show` | 输出格式改:`cost_24h_usd:` + `cost_active_usd:`,删 `cost used: $X.XX` 老行 |
| `crates/ccteam-core/tests/cost_summary_test.rs` | 5 个测试 |

### 关键设计

**`CostSummary`**:
```rust
#[derive(Debug, Clone, Serialize)]
pub struct CostSummary {
    pub cost_24h_usd: f64,
    pub cost_active_usd: f64,
    pub cost_total_usd: f64,
    pub session_count_24h: u32,
    pub session_count_active: u32,
}

pub fn cost_summary(
    slug: &str,
    progress_path: &Path,
    paths: &CcteamPaths,
) -> Result<CostSummary> {
    let now = chrono::Utc::now();
    let cutoff_24h = now - chrono::Duration::hours(24);

    let events = progress::read_all_events(progress_path)?;
    let agent_done_24h: Vec<_> = events.iter()
        .filter(|e| e["event"] == "agent_done")
        .filter(|e| parse_ts(e["ts"]) >= cutoff_24h)
        .collect();

    let cost_24h_usd: f64 = agent_done_24h.iter()
        .map(|e| e["cost_usd"].as_f64().unwrap_or(0.0))
        .sum();

    let active_sessions = progress::open_agent_spawns(progress_path)?;
    let cost_active_usd: f64 = active_sessions.iter()
        .filter_map(|s| s.job_id.as_ref())
        .map(|jid| claude_job::probe_state_json(jid).ok()
            .and_then(|v| v.cost_usd).unwrap_or(0.0))
        .sum();

    let cost_total_usd: f64 = events.iter()
        .filter(|e| e["event"] == "agent_done")
        .map(|e| e["cost_usd"].as_f64().unwrap_or(0.0))
        .sum();

    Ok(CostSummary {
        cost_24h_usd, cost_active_usd, cost_total_usd,
        session_count_24h: agent_done_24h.len() as u32,
        session_count_active: active_sessions.len() as u32,
    })
}
```

### 测试矩阵
- `t01_cost_summary_basic` — 5 agent_done events,cost 0.10 * 5 → cost_total = 0.50,cost_24h = 0.50
- `t02_cost_summary_24h_filter` — 一半 events ts > 24h 前 → 只算最近半
- `t03_cost_summary_active_reads_state_json` — 2 open agent_spawn,job_id 指向 mock state.json `cost_usd_total: 0.15` → cost_active = 0.30
- `t04_cost_used_usd_serde_compat_old_files` — 老 state.json 含 `cost_used_usd: 1.23` 仍 deserialize 不破
- `t05_doctor_update_hooks_removes_cost_accumulate` — settings.json 含老 hook → update-hooks 后消失

---

## 阶段 5:F84 budget cap

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs` | `WorkflowSpec` 加 `pub budget: Option<BudgetSpec>`;新结构体 `BudgetSpec { max_cost_usd_per_24h, max_agent_spawns_per_hour }` |
| `crates/ccteam-core/src/orchestrator.rs::event_loop` | 每个 tick 后(或 agent_done 处理后)调 `enforce_budget(slug)` |
| `crates/ccteam-core/src/orchestrator.rs` 新增 | `pub async fn enforce_budget(slug, spec) -> Result<BudgetVerdict>` |
| `crates/ccteam-core/src/orchestrator.rs` 新增 | `pub fn auto_disable_workflow(slug, reason) -> Result<()>` — 改 workflow.yaml `enabled: false` + 写 progress 事件 |
| `crates/ccteam-cli/src/commands.rs::run_show` | 输出加 `budget: $X.XX / $Y.YY (P% 24h)` 行 |
| `crates/ccteam-core/tests/budget_test.rs` | 4 个测试 |

### 关键设计

**`enforce_budget` 流程**:
```rust
async fn enforce_budget(&self, slug: &str, spec: &WorkflowSpec) -> Result<()> {
    let Some(budget) = &spec.budget else { return Ok(()); };
    let cost = cost_summary(slug, &progress_path, &self.paths)?;

    if let Some(cap) = budget.max_cost_usd_per_24h {
        if cost.cost_24h_usd >= cap {
            progress::append_event(&progress_path, &json!({
                "event": "budget_exceeded",
                "slug": slug,
                "kind": "cost_24h",
                "value": cost.cost_24h_usd,
                "cap": cap,
            }))?;
            self.auto_disable_workflow(slug, "budget_exceeded").await?;
            return Ok(());
        }
    }

    if let Some(rate_cap) = budget.max_agent_spawns_per_hour {
        let recent_spawns = count_agent_spawns_within(slug, &progress_path, Duration::hours(1))?;
        if recent_spawns >= rate_cap {
            progress::append_event(&progress_path, &json!({
                "event": "budget_exceeded",
                "slug": slug,
                "kind": "spawn_rate",
                "value": recent_spawns,
                "cap": rate_cap,
            }))?;
            self.auto_disable_workflow(slug, "spawn_rate_exceeded").await?;
        }
    }
    Ok(())
}
```

**`auto_disable_workflow`**:用 yaml_edit crate 写 `enabled: false` 到 workflow.yaml(F82 watcher 会 pick up + cancel loop)。

### 测试矩阵
- `t01_budget_cost_24h_trips` — 累 cost 0.52 / cap 0.50 → budget_exceeded + auto disable
- `t02_budget_spawn_rate_trips` — 1h 内 101 spawn / cap 100 → 同
- `t03_no_budget_no_op` — `budget` 字段缺 → 永不 trip
- `t04_disabled_then_reenabled_immediate_retrip` — disable 后 user 改回 enabled=true,24h 窗口仍超 → 5s 内再 trip

---

## 阶段 6:F86 daemon graceful shutdown

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/orchestrator.rs` | 加 `pub fn shutdown(&self) -> impl Future`;主 loop select arm |
| `crates/ccteam-cli/src/main.rs::run_stop` | 改:写 `/tmp/ccteam-<user>.shutdown` 触发文件 + poll pidfile 30s timeout,SIGKILL fallback |
| `crates/ccteam-cli/src/commands.rs::run_start` | daemon 主 loop 加 inotify on `/tmp/ccteam-<user>.shutdown` + SIGTERM handler |
| `crates/ccteam-core/tests/graceful_shutdown_test.rs` | 3 个测试 |

### 关键设计

**主 loop 改 select**:
```rust
let mut shutdown_rx = shutdown_token.subscribe();
loop {
    tokio::select! {
        _ = shutdown_rx.changed() => {
            tracing::info!("graceful shutdown begin");
            self.cancel_all_event_loops().await;  // 走 F82 cancel token
            let _ = tokio::time::timeout(Duration::from_secs(30),
                self.task_set.join_all()).await;
            return Ok(());
        }
        // ... existing arms (artifact events, manual triggers, etc.)
    }
}
```

`cancel_all_event_loops`:遍历 `cancel_handles`,每个发送 cancel,写 `workflow_done reason="shutdown"`。

### 测试矩阵
- `t01_stop_triggers_workflow_done_shutdown` — start + 1 rostered project + stop → progress.jsonl 末尾有 `workflow_done reason="shutdown"`
- `t02_stop_30s_timeout_falls_back_to_abort` — mock event_loop hangs → 30s 后 abort_all fallback,daemon exit 0
- `t03_sigterm_equivalent_to_stop` — kill -SIGTERM daemon pid → 同 `ccteam stop` 路径

---

## 阶段 7:F85 `~/.claude/jobs/` GC

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/claude_job.rs` | 加 `pub fn gc_terminated_jobs(retention_days: u32, dry_run: bool) -> Result<GcReport>` |
| `crates/ccteam-core/src/config.rs` | `Config` 加 `pub claude_jobs_retention_days: u32 #[serde(default = "default_jobs_retention")]`(default 7) |
| `crates/ccteam-core/src/orchestrator.rs::new` | 异步 `tokio::spawn(async { gc_terminated_jobs(...).await })` 启动时一次 |
| `crates/ccteam-cli/src/main.rs` | `Command::Doctor { gc_claude_jobs: bool, apply: bool }` flag |
| `crates/ccteam-core/tests/claude_jobs_gc_test.rs` | 4 个测试 |

### 测试矩阵
- `t01_gc_removes_terminated_old` — mock jobs/ 3 entries:1 working / 1 completed 8d 前 / 1 completed 3d 前 → 只删 8d 前那个
- `t02_gc_preserves_working` — state == "working" 不动
- `t03_gc_preserves_corrupt_state_json` — invalid JSON 不删 + WARN
- `t04_gc_zero_retention_noop` — `claude_jobs_retention_days: 0` → 不 GC

---

## 阶段 8:F87 clap allow_hyphen_values

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/main.rs` | `Command::Send.message` + `Command::Spawn.prompt` 加 `#[arg(allow_hyphen_values = true)]` |
| `crates/ccteam-cli/tests/leading_hyphen_test.rs` | 2 个测试 |

### 测试矩阵
- `t01_send_accepts_leading_hyphen` — `ccteam send dex-ui "--help"` 不触发 help
- `t02_send_dash_dash_separator_still_works` — `ccteam send dex-ui -- "--help"` 老写法兼容

---

## 阶段 9:F88 web bearer token clipboard

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/commands.rs::run_start` | 输出 token 后 try clipboard write |
| 新文件 `crates/ccteam-cli/src/clipboard.rs` | `pub fn copy_to_clipboard(s: &str) -> Result<&'static str>` — return platform name on success |
| `crates/ccteam-cli/src/main.rs` | `Command::Start { no_clipboard: bool }` flag |

### 关键设计

```rust
pub fn copy_to_clipboard(s: &str) -> Result<&'static str> {
    let candidates: &[(&str, &[&str])] = &[
        ("xclip", &["xclip", "-selection", "clipboard"]),
        ("wl-copy", &["wl-copy"]),
        ("pbcopy", &["pbcopy"]),
        ("clip.exe", &["clip.exe"]),
    ];
    for (name, argv) in candidates {
        if let Ok(mut child) = std::process::Command::new(argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(s.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return Ok(name);
            }
        }
    }
    bail!("no clipboard provider available")
}
```

无新测试(平台依赖,实测在 host)。

---

## 阶段 10:F89 CLI 瘦身 + `internal` 子命令分组

### 改动文件

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/main.rs` | 删 `Command::Phase` / `Command::Decisions` / `Command::Watchdog` enum branch + 各 run fn |
| `crates/ccteam-cli/src/main.rs` | 加 `Command::Internal { cmd: InternalCommand }` enum |
| `crates/ccteam-cli/src/main.rs` | `InternalCommand` 子枚举包 Hook / McpServe / Spawn / Send / Peek / Attach / Progress / Resume |
| `crates/ccteam-cli/src/main.rs::main` | 老顶层名(`hook` / `spawn` 等)接受但 stderr WARN "deprecated, use `ccteam internal X`,V0.5 删";routing 转发 |
| `crates/ccteam-cli/src/commands.rs` | settings.json hook 模板生成路径用 `ccteam internal hook ...`(也兼容老路径) |
| `skills/ccteam-control/SKILL.md` | 文档示例改新路径 |
| `crates/ccteam-core/src/mcp_serve.rs` | MCP server tool 实现内部调用 binary path 不变(用 lib API 不 shell out)|
| `crates/ccteam-cli/tests/cli_surface_test.rs` | 4 个测试 |

### 关键设计

**`Command` enum**(V0.4.6 后):
```rust
pub enum Command {
    Init { .. },
    Start { .. },
    Stop,
    New { .. },
    Ls { .. },
    Status { .. },
    Show { .. },
    Doctor { .. },
    Web { .. },
    Remove { .. },           // F81
    Internal { #[command(subcommand)] cmd: InternalCommand },
    // 老顶层名作为 hidden alias(deprecated WARN)
    #[command(hide = true)] Hook { .. },
    #[command(hide = true)] McpServe,
    #[command(hide = true)] Spawn { .. },
    #[command(hide = true)] Send { .. },
    #[command(hide = true)] Peek { .. },
    #[command(hide = true)] Attach { .. },
    #[command(hide = true)] Progress { .. },
    #[command(hide = true)] Resume { .. },
}

pub enum InternalCommand {
    Hook { #[command(subcommand)] cmd: HookCommand },
    McpServe,
    Spawn { .. },
    Send { .. },
    Peek { .. },
    Attach { .. },
    Progress { .. },
    Resume { .. },
}
```

### 测试矩阵
- `t01_help_user_facing_only` — `ccteam --help` 列 10 commands(9 user + 1 `internal`),不见 hook/spawn/peek
- `t02_internal_help_lists_subcommands` — `ccteam internal --help` 列 8 个
- `t03_legacy_top_level_still_works_with_warn` — `ccteam hook progress-append` 返回 0 + stderr WARN
- `t04_v03_legacy_commands_removed` — `ccteam phase show` 返回 non-zero + "removed in V0.4.6"

---

## 阶段 11:F90 Web WorkflowView 增强

### 改动文件 — 后端

| 文件 | 改动 |
|---|---|
| `crates/ccteam-web/src/routes/api_v1.rs` | 新 4 个 endpoint:`GET /api/v1/projects/<slug>/artifact_queue` / `cost_history` / `jobs/<job_id>/log` / `sessions/active` |
| `crates/ccteam-web/src/routes/api_v1.rs` 新 fn | `handle_artifact_queue` / `handle_cost_history` / `handle_job_log` / `handle_active_sessions` |
| `crates/ccteam-core/src/queries.rs` 新 fn | `pub fn artifact_queue(slug, project_dir, spec) -> Vec<QueueState>` — fs::read_dir each watch path |
| `crates/ccteam-core/src/queries.rs` 新 fn | `pub fn cost_history_buckets(progress_path, window: Duration) -> Vec<(DateTime, f64)>` |
| `crates/ccteam-web/tests/api_v1_workflow_panels_test.rs` | 4 个测试 |

### 改动文件 — 前端

| 文件 | 改动 |
|---|---|
| `crates/ccteam-web/web/src/pages/WorkflowView.tsx` | 顶部已有 agent cards → 每张 card 展开 session 列表(job_id / age / cwd / cost) |
| `crates/ccteam-web/web/src/components/ArtifactQueuePanel.tsx` 新 | watch path × queue count + oldest age |
| `crates/ccteam-web/web/src/components/EventsTimelinePanel.tsx` 新 | progress.jsonl tail SSE 实时 |
| `crates/ccteam-web/web/src/components/FailureInspector.tsx` 新 | errored card click → modal showing tail of output.log |
| `crates/ccteam-web/web/src/components/CostSparkline.tsx` 新 | SVG 24h + 7d sparkline |
| `crates/ccteam-web/web/src/pages/WorkflowView.tsx` | 拼装上述四个新组件 |
| `crates/ccteam-web/web/src/pages/WorkflowView.test.ts` | 加 4 个 SPA 单测 |

### 测试矩阵
- 后端:
  - `t01_artifact_queue_lists_watch_paths_with_age` — mock fs `.ccteam/foo/` 3 files → endpoint 返 count=3 + oldest_age
  - `t02_cost_history_buckets_by_hour` — 5 agent_done events 散 24h → 24 个 hour bucket
  - `t03_job_log_returns_tail` — mock jobs/<id>/output.log 1000 行 → `?tail=200` 返末 200
  - `t04_active_sessions_with_state_json_cost` — 2 open spawn → real-time state.json cost reported
- 前端:
  - `t01_workflow_view_renders_session_list_per_agent`
  - `t02_artifact_queue_panel_renders_three_paths`
  - `t03_events_timeline_subscribes_sse`
  - `t04_cost_sparkline_renders_24h_path`

---

## 整体:CLAUDE.md baseline 更新

- §一 表格:`workspace version` 改为 `0.4.6`
- `当前 next` 移走 V0.4.6 已完成(F81-F91 全 ship),留下:真 cron / Codex bg-job / workflow.yaml 条件分支 / WSL inotify 根治 / claude-mem 深度集成
- 测试 baseline 加 ~48(F81=6 + F82=5 + F83=5 + F84=4 + F85=4 + F86=3 + F87=2 + F89=4 + F90=8 + F91=5)→ 大概 ~773/0
- §六 "易踩的坑" 加 V0.4.5 → V0.4.6 一次性迁移条目

## 不破红线 checklist

- [ ] `ccteam remove --force` 之外路径,绝不动用户业务代码(项目根除 `.ccteam/` + `.claude/agents/` + workflow.yaml 之外的内容)
- [ ] 守红线"永不主动 kill 长 session" — remove 检测 tmux + claude bg + running spawn,refusal 上手前不动
- [ ] workflow 热 reload 用 cancellation token,**绝不**用 `watcher_handle.abort()` 强杀(那会丢 in-flight 进度)
- [ ] `.env` 永远不删 — 密钥归用户
- [ ] F86 graceful shutdown 30s timeout 后才 SIGKILL fallback,默认走 cancel token 路径
- [ ] F84 budget cap 走 F82 cancel token,**不** 用 abort_all
- [ ] F89 老 CLI 路径(`ccteam hook ...`)V0.4.6 保留 + WARN,V0.5 才删 — 兼容期一版
- [ ] F90 SessionDetail / TerminalView / BtwForm / KeyboardFab / MobileTerminalToolbar 等 V0.3 SPA 组件 **保留**(Codex tmux adapter 后续复用,不是 dead code)
- [ ] F91 `state.cost_used_usd` serde compat 保留(老 state.json 不破)
- [ ] F85 `~/.claude/jobs/` GC **不动** state == "working" 的目录

## 升级路径(用户视角)

V0.4.5 → V0.4.6 一次性迁移(`ccteam doctor` 提示):
1. `ccteam doctor --migrate-workflow-to-ccteam-dir --apply` — 把 N 个项目的 workflow.yaml 从 root 移到 `.ccteam/`(F83)
2. `ccteam doctor --update-hooks` — 清现有项目 settings.json 里的 cost-accumulate hook + 切到 `ccteam internal hook ...`(F89 / F91)
3. `ccteam doctor --gc-claude-jobs --apply` — 一次性 host 大扫除(F85,host 289 entries → ~10)
4. (无需 daemon 重启 — F82 热 reload 直接识别新位置)
5. `ccteam doctor` 走完后告知:
   - 删项目用 `ccteam remove <slug>` (F81)
   - 暂停项目用 workflow.yaml 改 `enabled: false`(F82)
   - 加 cost 守门用 workflow.yaml 加 `budget:` 段(F84)

V0.4.5 用户没用新功能 → 0 break。老 CLI 命令(`ccteam hook ...`)V0.4.6 仍工作 + WARN,V0.5 删。

## 并行子代理派工建议

11 个 finding 拆为两 wave:
- **Wave 1**(无依赖,可同时跑 7 worktrees):F82, F83, F85, F87, F88, F89, F91
- **Wave 2**(依赖 wave 1):F81 (用 F82), F84 (用 F82+F91), F86 (用 F82), F90 (用 F91)

每个 worktree 用 `git worktree add -b ccteam-v046-fNN /tmp/ccteam-v046-fNN origin/main` 起,subagent briefing 含本 PRD section + dev-plan 阶段 + 测试矩阵。

主 session 走 PR review/fix/merge 流程,最后 cargo bump 0.4.5 → 0.4.6 commit subject `v0.4.6:`,CLAUDE.md baseline 回填。
