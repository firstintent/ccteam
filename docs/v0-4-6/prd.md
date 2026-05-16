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

## 实现顺序 & 依赖

- **F82 先**:`enabled` + 热加载是 F81 `remove` 的底层依赖(remove 内部调 unroster)
- **F81 后**:用 F82 的 unroster API + 守红线 + purge 文件
- **F83 独立**:可与 F81/F82 并行;移到 `.ccteam/` 后 F82 inotify 监听路径也调整

整体 effort:F82 ~ 300 LOC + 5 tests,F81 ~ 200 LOC + 4 tests,F83 ~ 100 LOC + 2 tests + 1 migration helper。

## 不在 V0.4.6 范围

- `~/.claude/jobs/` GC(deferred 到 V0.4.7 — 跟 claude-bg lifecycle 关联更紧)
- 真 cron scheduler(deferred,需要单独子系统)
- web token clipboard(UX 小修)
- WSL inotify flake 根治(V0.4.5 F78 已部分缓解)
