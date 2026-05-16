# V0.4.6 — PRD

3 个 finding,按用户 2026-05-16 ask 顺序排。

---

## F81 — `ccteam remove <slug>` 项目生命周期管理

### 痛点
V0.4.5 没有"删除项目"的命令。一旦 `ccteam init` 把项目写进 `~/.ccteam/config.yaml::projects[]`,只能手工:

```bash
# 手工流程(易错,daemon 还需要重启才热剔除)
ccteam stop
# 手工编辑 ~/.ccteam/config.yaml 删 projects[] 条目
rm -rf <project>/.ccteam <project>/.claude/agents <project>/workflow.yaml
ccteam start
```

daemon log 实际 WARN 过 ghost entry,提示用户跑 `ccteam abandon <slug>`,**但这命令不存在**(子代理 2026-05-16 实证)。

### 需求
新增子命令:

```
ccteam remove <slug> [--purge] [--dry-run]
```

行为:
- **always**:从 `~/.ccteam/config.yaml::projects[]` 删该 slug;告知 daemon 热剔除(F82 wire);删 `~/.ccteam/progress/<slug>.jsonl`(或 flex 变体目录)、`~/.ccteam/inbox/<slug>/`、`~/.ccteam/control/<slug>/`(如有)。
- **`--purge`**:同时 `rm -rf <project>/.ccteam` + `<project>/.claude/agents` + `<project>/workflow.yaml`(以及 `.ccteam/workflow.yaml`,见 F83)。**不动业务代码**(项目根的其他文件)。
- **`--dry-run`**:打印会改的内容,不动文件。
- 操作前 refusal 条件(守 CLAUDE.md §三红线):
  - 项目里有活的 tmux session(`tmux ls | grep ccteam-<slug>`)→ refuse + 提示用户先 `tmux kill-session -t <sn>`
  - 项目里有活的 claude bg job(`~/.claude/jobs/<id>/state.json::cwd == <project>` 且 `state == working`)→ refuse + 提示
  - 项目里有正在跑的 `agent_spawn` 没匹配 `agent_done`(progress.jsonl tail)→ refuse + 提示 `ccteam show <slug>` 看具体

### 验收
1. `ccteam remove --dry-run dex-ui` 打印 4 行:从 config 删 / 删 progress.jsonl / 删 inbox / 守红线检查通过。
2. `ccteam remove dex-ui` 后 `ccteam ls` 不见 dex-ui;`<project>/` 业务代码 + `.git/` 完好,只 `.ccteam/` + workflow.yaml + .claude/agents/ 没了(如 `--purge`)。
3. daemon log 出现 `event_loop terminated slug="dex-ui" reason="removed"`(F82 wiring)。
4. 残留:`<project>/.env`(用户密钥)始终保留,**永远不删** — 用户决定。

### 衍生子命令(可选)
`ccteam abandon <slug>`:仅清 config + ccteam-managed orchestration state,**不动**项目目录。给 `state.json 缺失但 config.yaml 还有条目` 的孤儿情况用。事实上 `ccteam remove --purge=false`(默认 NOT purge)就是这个;不必再加新动词。

---

## F82 — workflow.yaml `enabled` 开关 + 热加载

### 痛点
1. 项目 workflow.yaml 改了一字段 → 必须 daemon stop/start,影响其他项目
2. 想临时停某个项目的 loop → 现在只能 `mv workflow.yaml workflow.yaml.disabled-xxx`(子代理 2026-05-16 用过这招)+ 等下次 daemon 启动才生效。已 roster 的 event_loop 不会停。
3. 改 trigger 路径(如把 `watch:.ccteam/issues/` 换成 `watch:.ccteam/fix-requests/`)→ 新 daemon 起来才注册新 inotify,老 daemon 仍监老路径

### 需求
1. **`enabled: bool` 顶层字段**(default `true`)— 加在 `WorkflowSpec`:
   ```yaml
   name: dex-ui-autoloop
   enabled: false        # 项目存在但 loop 暂停
   agents:
     ...
   ```
2. **daemon 监听 workflow.yaml 文件本身的 mtime + 内容 hash**:
   - 启动时:为每个 rostered 项目,装一个 inotify watch on `<project>/workflow.yaml`(或 F83 后是 `<project>/.ccteam/workflow.yaml`)
   - 改动 → 解析新 spec → diff 老 spec:
     - `enabled: false` → 优雅终止老 event_loop(JoinSet.abort_handle()),进度写 `workflow_done` 事件 reason="disabled"
     - `enabled: true` 且老 loop 在 → 替换 spec,如果 trigger 变了重装 watcher
     - `agents` 拓扑变 → 终止老 loop + 重启新 loop(干净)
3. **`ccteam remove`** 调内部 `unroster(slug)` API,等价于设 `enabled: false` 但同时从 `tasks: JoinSet` 中移除

### 实现要点
- `WorkflowSpec` 添加 `pub enabled: bool` (`#[serde(default = "default_true")]`)
- `Orchestrator` 加 `pub fn unroster_project(slug)` + `pub fn reload_project(slug)`
- `spawn_new_rostered_projects` 在 spawn 前 check `spec.enabled`
- 新 inotify watcher: 每个 rostered 项目一个 watch,target = workflow.yaml path
- 终止语义:**abort_handle()** 不 graceful,导致 in-flight session 被弃。改用 `mpsc::oneshot` cancellation token,event_loop 在 select! 中等 token → 收到后写 `workflow_done` 事件 + clean exit

### 验收
1. 项目 workflow.yaml 改 `enabled: false` → daemon log 5s 内出现 `event_loop disabling slug="X"`,inotify 卸载,progress.jsonl 加 `workflow_done reason="disabled"`。
2. 改回 `enabled: true` → 5s 内 `event_loop starting slug="X"`,watch register 重出。
3. 改 trigger 路径(`watch:foo/` → `watch:bar/`)→ 5s 内 daemon log 旧 watch removed,新 watch registered,无 daemon restart 需要。
4. workflow.yaml syntax error → daemon log WARN,**保留**老 loop 不动(fail-safe)。

---

## F83 — workflow.yaml 位置迁移到 `.ccteam/`

### 痛点
当前 V0.4.5 接受两个位置:
- `<project>/workflow.yaml`(root,推荐)
- `<project>/.ccteam/workflow.yaml`(也接受)

ccteam-creator skill 教用户写到 root。但 root 上有别的(用户业务代码 / Cargo.toml / package.json / etc.),workflow.yaml 跟它们混在一起视觉上突兀,提交时也容易误 git add。

`.ccteam/` 已经是 orchestration state SoT(state.json / config.json / progress.jsonl / artifacts / rules /...),workflow.yaml 也该在那里。

### 需求
1. **新建项目**:`ccteam init` / `ccteam new` 写到 `<project>/.ccteam/workflow.yaml`(不再 root)
2. **`.gitignore` 整段已经包含 `.ccteam/`** — workflow.yaml 自然 gitignored(orchestration state 不入业务库,正合 CLAUDE.md §三红线)
3. **read 优先级**:`<project>/.ccteam/workflow.yaml` > `<project>/workflow.yaml`(旧位置 fallback,V0.5 删)
4. **migration**:`ccteam doctor --migrate-workflow-to-ccteam-dir`(可选,默认 dry-run)— 把根上的 workflow.yaml 移到 `.ccteam/` 下;ccteam-creator skill 文档同步
5. **新建项目的提示**:`ccteam init` 完毕后告诉用户"workflow.yaml 在 `.ccteam/workflow.yaml`,改这里调拓扑"(避免用户去根目录找)

### 验收
1. `ccteam new test-foo --team dev` 生成的 workflow.yaml 在 `~/projects/dev-test-foo/.ccteam/workflow.yaml`,**不在**项目根
2. 旧项目跑 `ccteam doctor --migrate-workflow-to-ccteam-dir --apply` → 根上的 workflow.yaml 移到 `.ccteam/`,daemon 5s 内识别(F82 wiring)
3. `ccteam-creator` skill 的 workflow.yaml 路径说明更新到新位置(`docs/interfaces.md` 也同步)
4. 既有 root workflow.yaml 不破:fallback read 通过

---

---

## F84 — `max_cost_usd_per_24h` budget cap

### 痛点
V0.4.5 落地后 dex-ui 自激励 loop 4h 烧 $1.10(2026-05-16 实证):explorer agent 写文件到 `.ccteam/backlog/`,又 trigger 自己 → 无穷循环。没有 budget cap,只能靠人发现 + 手动 `mv workflow.yaml workflow.yaml.disabled`。`watchdog scan` 的 $200 物理上限是 CLAUDE.md §三红线**全 ccteam 进程**而非 per-project,粒度太粗。

### 需求
1. **`workflow.yaml` 顶层加 `budget` 段**:
   ```yaml
   name: dex-ui-autoloop
   enabled: true
   budget:
     max_cost_usd_per_24h: 5.00         # 过去 24h 累加 cost 超此值 → 自动 enabled=false
     max_agent_spawns_per_hour: 100     # 过去 1h spawn 数超此值 → 同上 (防 self-excitation runaway)
   ```
2. **orchestrator event_loop 每个 tick 后**:用 F91 收敛后的 cost 来源(`workflow_summary.cost_24h_usd`)做 budget check;超限 → 写 `budget_exceeded` 事件 + 自动改 workflow.yaml `enabled: false`(走 F82 cancellation token 优雅退出)+ 写 `workflow_done reason="budget_exceeded"`
3. **`ccteam show` 输出加 budget 利用率**:`budget: $0.42 / $5.00 (8% 24h)` 让用户看到接近阈值

### 验收
1. workflow.yaml 设 `budget.max_cost_usd_per_24h: 0.50` + 任意 agent run 累计超 $0.50 → 5s 内 daemon log `budget_exceeded slug="X" cost_24h=0.52 cap=0.50`,workflow.yaml `enabled: false`,progress.jsonl 加事件。
2. 用户改回 `enabled: true` 但不调高 cap → 5s 内再次 trip(因为 24h 滑窗内仍超),log WARN。
3. `max_agent_spawns_per_hour` 类似:超阈值 → 同样 trip,reason="spawn_rate_exceeded"。
4. 默认 budget(`budget` 字段缺) → no-op,与 V0.4.5 行为一致。

---

## F85 — `~/.claude/jobs/` GC

### 痛点
host 残留 289 entries(2026-05-16 sweep)。claude bg job 完成后 state.json + output.log 不自动清,长期堆积 → inode 压力 + 翻日志难。F80 phantom cleanup 只动 progress.jsonl,不动 `~/.claude/jobs/`。

### 需求
1. **daemon 启动时 GC 扫描**(`Orchestrator::new` 异步任务):
   - 扫 `~/.claude/jobs/*/state.json`,parse `state` + `firstTerminalAt`
   - `state` ∈ {`completed`, `stopped`, `error`, `killed`} 且 `firstTerminalAt` > 7 天前 → 整个 `~/.claude/jobs/<id>/` 目录 `rm -rf`
   - **不动** `state == "working"`(可能 ccteam 不在线但 daemon 在跑)
   - **不动** state.json parse 失败的(写 WARN,留人工排查)
2. **可调 retention**:`~/.ccteam/config.yaml` 加 `claude_jobs_retention_days: 7`(default 7),0 = 禁用
3. **`ccteam doctor --gc-claude-jobs [--apply]`** 手动触发(dry-run by default)
4. **GC report 写 progress 全局事件**:`{event: "claude_jobs_gc", removed: N, dir_count_before: 289, dir_count_after: 12}` 到 `~/.ccteam/progress/_global.jsonl`

### 验收
1. host 跑 `ccteam doctor --gc-claude-jobs --apply` → 289 entries 减到只剩 7d 内 + working 的(预计 ~10 个)
2. daemon 启动时 background 跑一次 GC → log `claude_jobs gc start; removed N` 一行
3. `claude_jobs_retention_days: 0` → daemon 启动不 GC
4. `state.json` 损坏(invalid JSON)→ 留着 + WARN,不 rm

---

## F86 — daemon graceful shutdown

### 痛点
V0.4.5 daemon 收到 SIGTERM 时直接 `JoinSet.abort_all()`,所有 event_loop 硬中断 → in-flight session 不写 `workflow_done`,下次启动 F80 phantom cleanup 才补 synthetic agent_done。**根因还在**(F80 只缓解症状)。CLAUDE.md §一 "当前 next" 列出根治。

### 需求
1. **`Orchestrator` 加 `shutdown_token: tokio::sync::Notify`**(或 `watch::Sender<bool>`)
2. **CLI `ccteam stop`** 不再 `kill PID` + poll pidfile,改:
   - 写 `/tmp/ccteam.shutdown`(或经 unix socket / signal-fd)→ daemon 主循环 select 收到 → trigger Notify
   - daemon 主循环跑 `shutdown_token.notified().await` arm → cancel 所有 event_loop(用 F82 cancellation token,workflow_done reason="shutdown") → JoinSet `join_all()` 等所有 task 真正退出
   - timeout 30s 后才走 abort_all() fallback
3. **保留 SIGTERM/SIGINT 兼容**:linux signal handler 也 trigger shutdown_token(双触发兼容 systemd / docker stop)

### 验收
1. daemon 启动 + 3 个项目 roster → `ccteam stop` → progress.jsonl 三个 `workflow_done reason="shutdown"`,daemon exit code 0,无 stale spawn 残留(下次启动 F80 cleanup 找不到东西清)
2. daemon 卡死场景(其中一个 event_loop 在 await 永远不返回):`stop` 30s timeout → log WARN + abort_all() fallback,exit code 0
3. SIGTERM(`systemctl stop ccteam`)等价 `ccteam stop`,行为一致

---

## F87 — clap `allow_hyphen_values`

### 痛点
`ccteam send "<slug>" -- --help` 想给 agent session 发 `--help` 字符串,但 clap 默认把 `--help` 解读为 ccteam 自己的 help flag → exits。CLAUDE.md §一 "当前 next" 列出。

### 需求
1. `Command::Send` / `Command::Spawn` 的位置参数(`message: String` / `prompt: String`)加 `#[arg(allow_hyphen_values = true)]`
2. `Command::Hook { cmd }` 已经是 enum,无需改
3. doc 更新:`ccteam send "<slug>" "--help"` 不再需要 `--`,clap 直接接受

### 验收
1. `ccteam send dex-ui "--help"` → 字符串 `--help` 发到 agent stdin,不触发 ccteam help
2. `ccteam send dex-ui -- "--help"`(老写法)仍兼容
3. `ccteam --help` 本身不破

---

## F88 — web bearer token 自动 clipboard

### 痛点
`ccteam start` 输出 `web bearer token: xxx` 行,用户要复制 → 鼠标手动选 → 粘到浏览器。Web token 一旦 daemon 重启就变,体验差。CLAUDE.md §一 "当前 next" 列出。

### 需求
1. **`ccteam start` 后输出 token 时尝试 clipboard write**:
   - Linux:`xclip -selection clipboard` / `wl-copy`(Wayland)
   - macOS:`pbcopy`
   - WSL:`clip.exe`
   - 都不可用 → 静默 + 仍打印 token 文本(不 fail)
2. **加 `--no-clipboard` flag** 给 CI / 不需要的场景
3. **打印格式**:
   ```
   ccteam web running at http://localhost:8400
   token: xxx (copied to clipboard)
   ```
   或 `(clipboard unavailable; copy manually)`

### 验收
1. host(WSL)跑 `ccteam start` → token 自动到 Windows clipboard(经 `clip.exe`)
2. headless server(无 X / Wayland)→ 打印 fallback message,start 不 fail
3. `--no-clipboard` → 不试,直接打印

---

## F89 — CLI 瘦身 + 内部命令藏 `internal` 分组

### 痛点
V0.4.5 CLI 20 子命令,用户 `ccteam --help` 看到的:
```
init, start, stop, new, ls, status, show, attach, peek, progress,
resume, send, spawn, decisions, doctor, hook, mcp-serve, phase,
watchdog, team, session, web
```
混着三类:
- **用户日常**(9):`init / start / stop / new / ls / show / doctor / web / remove`(F81 新增)
- **meta-agent / MCP 内部**(7):`hook / mcp-serve / spawn / send / peek / attach / status`(给 ccteam-control skill 和 MCP server 用,**不是**用户日常)
- **V0.3 phase-时代 legacy**(3):`phase / decisions / watchdog scan`(V0.4.0 workflow 模式后无用)
- **Codex tmux 共用**(1):`attach` 移到内部组但还活(Codex CLI adapter 路径用)
- **杂**:`progress / resume / team / session` 中 progress 是 status 别名

### 需求
1. **删 V0.3 legacy**:`Command::Phase` / `Command::Decisions` / `Command::Watchdog`(三个 enum branch + 对应 run_phase / run_decisions / run_watchdog 函数 + 帮助文)
2. **`internal` 子命令分组**:把 `hook / mcp-serve / spawn / send / peek / attach / progress / resume` 移到 `ccteam internal <subcmd>`,**API 不破**(MCP server / ccteam-control skill / hook installer 调 `ccteam internal hook progress-append` 等)
3. **migration shim**(V0.4.6 仅一版):老 `ccteam hook progress-append` 仍工作但打 deprecation WARN,V0.5 删
4. **`--help` 表现**:用户看到 9 个 user-facing 命令 + 1 个 `internal` 折叠提示

### 验收
1. `ccteam --help` 输出只列 9 个 user-facing + `internal`(底部一行 "Internal commands: ccteam internal --help")
2. `ccteam internal --help` 列内部 8 个子命令
3. `ccteam hook progress-append` 仍工作 + stderr WARN
4. `ccteam phase show` 报错 `unknown command(removed in V0.4.6 — see docs/v0-4-6/prd.md F89)`
5. settings.json 模板 + ccteam-control skill / doctor 生成的 hook command 用新路径(`ccteam internal hook ...`)
6. MCP server `tool_send` / `tool_peek` 内部 still call 老 binary path,V0.5 才迁

---

## F90 — Web WorkflowView 增强(SessionDetail / Terminal 保留)

### 痛点
V0.4.5 SPA(1080 LOC,4 pages):
- WorkflowView.tsx(360 行)只显示 agent cards + running_count + cost_24h(F80 加的 active dot)
- ProjectDetail.tsx 是 V0.3 session list,V0.4.0 后部分失效
- 看不到:每个 agent 实时 job_id / cwd / started_at / artifact queue 内容 / 最近 events / failure log / cost trend

**保留 tmux SessionDetail / TerminalView / BtwForm 等**:Codex CLI adapter 走 tmux 模式,这些组件复用,不算 dead code。

### 需求
1. **WorkflowView agent card 增强**:每张 card 显示
   - `running_count` + `queued_count` + `failed_count`(已有)
   - **新**: 当前 running session 列表(最多 5 行):`job_id` short hash / `started_at`(relative time) / `cwd` last component / 实时 cost(用 F91 的 claude state.json 来源)
   - 点 card → 展开 session 详情面板(SSE 推送)
2. **新加 "Artifact Queue" 面板**(WorkflowView 底部):
   - 每个 watch path(`spec.agents[].triggers[Trigger::Watch]`)展示
   - 待处理文件数 + 最旧文件 age(秒数)+ 最新文件名
   - 后端 `GET /api/v1/projects/<slug>/artifact_queue` 实时 `fs::read_dir` 计算
3. **新加 "Events Timeline" 面板**(WorkflowView 右侧):
   - 取 progress.jsonl 最近 100 行
   - 颜色编码:绿色 `agent_done`,橙色 `gate_triggered` / `budget_exceeded`,红色 `escalation` / errored agent_done
   - 已有 SSE infra(EventsLive.tsx)直接复用
4. **新加 "Failure Inspector"**:
   - errored agent card 点击 → 后端 `GET /api/v1/projects/<slug>/jobs/<job_id>/log?tail=200` → 渲染 `~/.claude/jobs/<job_id>/output.log` 尾部
   - read-only,不 PTY
5. **新加 "Cost Trend" mini sparkline**:
   - SVG 24h + 7d 两个 sparkline
   - 数据源:F91 收敛后的 `workflow_summary.cost_24h_usd` + 历史 `progress.jsonl::agent_done.cost_usd` aggregated by hour
   - 后端 `GET /api/v1/projects/<slug>/cost_history?window=24h|7d` → `[{hour: timestamp, cost: f64}]`

### 验收
1. WorkflowView 加载 dex-ui → 3 个 agent card(explorer/fixer/master)+ 每个 card 当前 2-3 running session 展示 job_id / age / cwd / cost
2. Artifact Queue 面板显示:`.ccteam/explore-requests/: 3 files (oldest 2m ago)`
3. Events Timeline 显示最近 events,SSE 推送新 event 实时插
4. errored agent card 点击 → log tail 弹面板(read-only)
5. Cost sparkline 显示 24h cost 曲线
6. tmux SessionDetail 路径不破(Codex adapter 后续复用)— 现有 SessionDetail / Terminal / Btw / Keyboard 组件 / API 全保留

---

## F91 — Cost SoT 收敛到 Claude state.json

### 痛点
V0.4.5 cost 数据三个来源:
1. **`state.cost_used_usd`**(per project,在 `~/.ccteam/projects/<slug>/state.json` 里)— 由 `Hook::CostAccumulate` 接收 stdin parse claude 输出累加
2. **`progress.jsonl::agent_done.cost_usd`**(per session,F66 hook 写)
3. **`claude_job::probe_job` 读 `~/.claude/jobs/<id>/state.json::cost_usd_total`**(F80 加)

任一 hook miss(claude --bg argv 漂移 / stdin parse 失败 / hook 没装)→ ccteam 端 cost 漂。**真实来源就是 Claude 自己写的 state.json**,ccteam 不该自己再算一份。

### 需求
1. **删 cost 累加路径**:
   - `Hook::CostAccumulate` enum branch 删
   - `ccteam_hooks::cost_accumulate` 函数删
   - `ccteam doctor --install-hooks` 模板里的 `cost-accumulate` hook 不再生成(`doctor --update-hooks` 同步清现有项目 settings.json)
2. **`state.cost_used_usd` 字段保留 serde compat**(用户决定:不 break 老 state.json):
   - `#[serde(default)]` 接受老文件
   - 写入路径:**不再 mutate**(标 `#[deprecated]` rust attr + comment "V0.5 删")
   - 读取路径:`workflow_summary` / `ccteam show` 不再用 `state.cost_used_usd`,改:
     ```rust
     pub struct CostSummary {
         pub cost_24h_usd: f64,      // sum over progress.jsonl::agent_done.cost_usd within 24h window
         pub cost_active_usd: f64,    // sum over live ~/.claude/jobs/<active>/state.json::cost_usd_total
         pub cost_total_usd: f64,     // cost_24h + historical aggregate
     }
     ```
     - 历史 cost 用 `progress.jsonl::agent_done.cost_usd`(F66 已经从 state.json 读的,只是 snapshot)
     - 实时 active cost 用 `claude_job::probe_job` reads `state.json::cost_usd_total` 直接
3. **F90 Cost sparkline 用此新源**

### 验收
1. `ccteam show dex-ui` 输出 `cost_24h_usd: $0.42 (5 sessions)` + `cost_active_usd: $0.08 (2 running)`,不再有 `cost used: $X.XX` 老行
2. `~/.claude/settings.json` 里 `cost-accumulate` hook 自动消失(doctor --update-hooks)
3. 删 hook 后所有现有项目 cost 数字仍准(从 progress.jsonl + state.json 两源算)
4. `state.cost_used_usd` 在 state.json 里**不变**(老值)— serde compat,不破老文件
5. F84 budget cap 用 `cost_24h_usd` 做判定

---

## 实现顺序 & 依赖

```
F82 (cancel token)  ←─┬── F81 (remove 用 unroster)
                      ├── F86 (graceful shutdown 用 cancel token)
                      └── F84 (budget exceeded → enabled=false → cancel)

F91 (cost SoT)  ←──── F84 (budget 判定用 cost_24h)
                └──── F90 (Cost sparkline 数据源)

F83 (workflow → .ccteam/)  独立,可与上面并行;F82 watcher path 跟着调整

F89 (CLI 瘦身)  独立
F85 (jobs/ GC) 独立
F87 (allow_hyphen)  独立
F88 (clipboard)  独立
```

建议子代理派工顺序(并行 8 worktrees):
- **wave 1**(无依赖,可同时跑):F82, F83, F85, F87, F88, F89, F91
- **wave 2**(依赖 wave 1):F81(用 F82), F84(用 F82 + F91), F86(用 F82), F90(用 F91)

整体 effort 估算:
| finding | LOC | tests |
|---|---|---|
| F81 | ~200 | 6 |
| F82 | ~300 | 5 |
| F83 | ~100 | 5 |
| F84 | ~150 | 4 |
| F85 | ~120 | 4 |
| F86 | ~80 | 3 |
| F87 | ~20 | 2 |
| F88 | ~60 | 2 |
| F89 | ~250(大部分是 mv enum branch + help text) | 4 |
| F90 | ~600(SPA 改 + 4 API endpoint) | 8 (SPA + API) |
| F91 | ~180(主要是删 + workflow_summary 改) | 5 |
| **合计** | **~2060 LOC** | **48 tests** |

## 不在 V0.4.6 范围

- 真 cron scheduler(deferred,需要单独子系统)
- Codex CLI argv 标准化(F62 推迟)
- Codex bg-job 形态(独立 adapter,大动作)
- workflow.yaml 条件分支(设计性,V0.5)
- WSL inotify flake 根治(V0.4.5 F78 已部分缓解;host 不缺资源,影响仅 dev WSL)
- claude-mem 深度集成(strategic doc §3.7 已声明永远 optional)
