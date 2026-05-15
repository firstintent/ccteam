# PRD V0.4.2 — One install command + global config consolidation + hotfix sweep

> Patch round on top of V0.4.1. Two **new user-facing features** + two
> **hotfixes** already shipped on `main`. Doc-first; code lands per
> F-finding after user review.
>
> base = `origin/main` HEAD,workspace.version 0.4.1 → bump `0.4.2`
> 在 F-finding 全部 ship 后。

---

## 0. 本轮 scope

| # | 内容 | 类型 | 状态 |
|---|---|---|---|
| F70 | mcp-serve SIGTERM/idle/orphan 退出 deadlock 用 `std::process::exit(0)` | Hotfix | ✅ ship 在 `2dd18ea` |
| F71 | Web bind default `0.0.0.0:7331`(host 部署默认 LAN 可达,token auth on) | Hotfix | ✅ ship 在 `2025dcc` |
| **F72** | **`ccteam init` 三合一:新项目 / 已有目录 / 已有 ccteam 项目 都一个命令** | **新功能** | 待 review/实现 |
| **F73** | **`~/.ccteam/config.yaml` 统一全局配置(projects_root + projects registry)** | **新功能** | 待 review/实现 |

F70/F71 已经 ship 在 main 上。F72/F73 是本轮的需求。

---

## 1. F72 — `ccteam init` 一个命令处理三种安装场景

### 1.1 当前状态(混乱)

| 场景 | 今天的命令 | 痛点 |
|---|---|---|
| 全新项目 | `ccteam new "<request>"` | LLM 自动起 slug,黑盒;只能落 `~/projects/<team>-<slug>/` |
| 已有 git repo | 没有 — 上一版 PRD 提议 `ccteam adopt` | 用户反馈"太复杂" |
| 已有 ccteam 项目重装 | 没有官方流程 | 用户手动覆盖 / `ccteam doctor --install-meta-agent`(只装 meta)|
| 工具自身 install | `ccteam init`(skill / MCP / meta-agent wizard) | 名字占用,但只装"周边工具",不装项目 |

### 1.2 新设计 — 一个 `ccteam init` 涵盖一切

`ccteam init` 统一处理:
- **首次跑机器**:装 skill + MCP + meta-agent(同 V0.4.1 wizard)
- **首次跑某 cwd**:在 cwd 写 `.ccteam/` + `.claude/` 骨架,append 到 `~/.ccteam/config.yaml::projects[]`
- **已 ccteam'd cwd 再跑**:idempotent refresh(参考 V0.4.1 `bootstrap_meta_project` 的 refresh 语义)

#### 用法

```bash
# 场景 A:全新项目,新 dir
mkdir ~/projects/myapp && cd ~/projects/myapp
ccteam init                                  # slug = cwd basename = "myapp", team = dev

# 场景 B:已有 git repo 原地装
cd ~/code/my-fastapi-app
ccteam init                                  # slug = "my-fastapi-app"
ccteam init --slug fastapi --team dev        # 显式 slug + team override
ccteam init --workflow review                # 用预置 workflow 模板填初稿

# 场景 C:已经装过 ccteam,想 refresh / 升级
cd ~/code/my-fastapi-app
ccteam init                                  # 默认 idempotent:保留 workflow.yaml + agents/*.md
ccteam init --force                          # 全覆盖
ccteam init --reset-agents                   # 只重写 .claude/agents/*.md,workflow.yaml 保留

# 场景 D:用 slug 而不是 path —— 远端创建
ccteam init --in <projects_root>/myapp       # mkdir + cd + init,等价 A
```

#### `--in <path>` 简便选项

```bash
# 不想 mkdir + cd 两步的用户:
ccteam init --in ~/projects/my-new-app       # 等价: mkdir -p ~/projects/my-new-app && cd ~/projects/my-new-app && ccteam init
```

如果 `<path>` 是相对路径,resolve 相对当前 cwd。如果不存在,创建。如果已经是 ccteam 项目,走 refresh 流程。

### 1.3 默认覆盖策略

`ccteam init` 在已有 ccteam 项目里重跑时:

| 文件 | 默认 | `--force` |
|---|---|---|
| `.ccteam/state.json` | refresh 字段(slug / team / tmux_session)保留 timestamps / cost | 重建 |
| `.ccteam/inbox/`, `outbox/` | 不动 | 不动(用户数据)|
| `.ccteam/workflow.yaml` | **保留**(用户手动 edit 的核心) | 覆盖 |
| `.claude/agents/*.md` | **保留**(用户 prompt) | 覆盖 |
| `.claude/settings.json` | refresh(只覆盖 ccteam-managed marker section,沿用 marker-protected pattern) | 全覆盖 |

`--reset-agents` 只重写 `.claude/agents/*.md`,其他保留 — 用户改坏了 agent 但 workflow 还想保留时用。

### 1.4 删 `ccteam new`?

`ccteam new "<request>"` V0.4.0 的 LLM-auto-slug 路径有以下问题:
- 黑盒 — slug 怎么来的不透明
- 多一个 LLM round trip,慢 + 烧 token
- 用户想要的 slug 通常很简短(`fastapi-app`、`react-spa`),`slugify_brief` 走规则就够

V0.4.2 处理:
- **保留** `ccteam new <slug>` 作为 thin wrapper(`ccteam init --in <projects_root>/<slug>`)— 老用户脚本不破
- **删** `ccteam new "<request>"` free-text 路径(连同 `auto_slug_model` / `--no-auto-slug` 全删)
- 文档推荐 `ccteam init`

### 1.5 边角情况

- **slug 冲突**:cwd basename 已经被注册表占用 → 提示用 `--slug <other-name>` rename;`--force` 不能跳过 slug 冲突
- **install 在 ccteam 仓库本身**:fail-loud(CLAUDE.md §六 红线)。detector:cwd 有 `Cargo.toml` + `crates/ccteam-cli/` 同时存在
- **install 在 home dir 或其他敏感路径**:warn + 要求 `--force`
- **非 git 目录**:warn 但允许(ccteam 不依赖 git)

---

## 2. F73 — `~/.ccteam/config.yaml` 统一全局配置

### 2.1 当前状态

- 全局 config:**没有**。`paths.projects_root` 只看 env `$CCTEAM_PROJECTS_ROOT` 否则 `$HOME/projects`。
- Projects registry:**没有**。daemon walk `paths.projects_root` 找 dir。
- Web token:`~/.ccteam/web-token`(独立文件)
- Watchdog:`~/.ccteam/watchdog.yaml`(独立文件)

### 2.2 新设计

**一个文件** `~/.ccteam/config.yaml`,包含**所有全局配置 + projects 注册表**:

```yaml
# ~/.ccteam/config.yaml — global ccteam config (V0.4.2 F73).
#
# Single source of truth for projects_root + adopted projects list +
# all other user-level preferences. Read by every CLI / daemon / MCP
# invocation. Env vars (CCTEAM_PROJECTS_ROOT etc.) still override
# values here for hermetic test harnesses.

# Canonical place where `ccteam init --in <slug>` lays down new
# project dirs. Default ~/projects.
projects_root: ~/projects

# All projects under ccteam management. Absolute paths. The list is
# the SoT for `ccteam ls`, `ccteam status`, and daemon roster —
# daemon NO LONGER walks projects_root filesystem.
#
# Entries are appended by `ccteam init` (any successful install)
# and pruned by `ccteam abandon <slug>` (V0.4.2 alias for removing
# from this list; --remove-files optionally cleans .ccteam/ too).
projects:
  - slug: myapp
    path: /home/rob/projects/myapp
    team: dev
    installed_at: 2026-05-15T14:00:00Z
  - slug: fastapi-app
    path: /home/rob/code/my-fastapi-app
    team: dev
    installed_at: 2026-05-15T16:30:00Z
  - slug: meta
    path: /home/rob/projects/meta
    team: meta-agent
    installed_at: 2026-05-10T09:00:00Z

# V0.4.2: existing watchdog / web settings folded in (instead of
# separate ~/.ccteam/watchdog.yaml + scattered env vars).
web:
  bind: 0.0.0.0:7331         # ccteam start --web-bind default (F71)
  no_auth: false
  token_file: ~/.ccteam/web-token   # token still lives in its own file (only secret)

watchdog:
  notify_on_cycle_count: 3
  # ... 其余字段从 watchdog.yaml 迁移
```

### 2.3 优先级(读取顺序)

1. CLI flag(`ccteam init --projects-root /new`)— immediate override + write back to config
2. env var(`$CCTEAM_PROJECTS_ROOT`)— ad-hoc / test override(不持久化)
3. `~/.ccteam/config.yaml` field
4. hardcoded default

### 2.4 Daemon 改动

`collect_projects(paths)` 行为变化:
- **旧**:walk `paths.projects_root`,每个 dir 加载 `state.json`
- **新**:read `config.yaml::projects[]`,每条 `{slug, path, team}` 加载 `<path>/.ccteam/state.json`

这把"在哪里"和"项目内容"解耦 — projects_root 现在只是 `ccteam init --in <slug>` 的 default base dir,**不再** 是发现机制。

V0.4.1 P1(daemon hot-reload via rescan)仍然工作 — 现在 rescan 读 config.yaml 而不是 walk fs。

### 2.5 迁移

V0.4.1 用户(`~/projects/<slug>/` 一堆已有项目)升级 V0.4.2:

```bash
ccteam doctor --migrate-v041-to-v042
# 自动扫 ~/projects/* 找 .ccteam/state.json,append 到新 config.yaml::projects[]
# warn: 找到 N 个项目,写入 config.yaml — review 后 commit
```

非 daemon-managed 文件(`~/.ccteam/watchdog.yaml` 等)的迁移:doctor 自动 fold,旧文件加 `.migrated` 后缀保留。

### 2.6 `~/.ccteam/` 目录文件版图(V0.4.2 后)

```
~/.ccteam/
├── config.yaml             # 单源全局配置(F73 新)
├── web-token               # 唯一秘密单独文件(F73 不动)
├── state/
│   ├── orchestrator.pid    # daemon pidfile
│   └── heartbeat
└── progress/
    └── <slug>.jsonl        # per-project 事件流(SoT 红线)
```

(`watchdog.yaml`、`projects.yaml` 等 V0.4.1 散文件 V0.4.2 一律合进 `config.yaml`)

---

## 3. 交叉影响

| 改动 | 影响位置 |
|---|---|
| `ccteam init` 扩展为 unified install | `crates/ccteam-cli/src/main.rs::Command::Init`,`crates/ccteam-cli/src/commands.rs::run_init` |
| `ccteam new` 简化为 `init --in` wrapper | `Command::New` 删 `auto_slug_*` flag;`run_new` 实现走 `run_init` |
| `~/.ccteam/config.yaml` 模块 | 新 `crates/ccteam-core/src/config.rs`(load / save / merge / migrate)|
| projects registry SoT | `queries::collect_projects` 改读 config;旧 walk fs 路径删 |
| `ProjectSummary::path` 字段 | 新增,反映 absolute path(canonical / external 都行)|
| `actions::*` 路径解析 | 走 `lookup_project(slug)` → path,不再 `paths.project_dir(slug)` |
| watchdog config 折进来 | `crates/ccteam-core/src/watchdog.rs` load 路径改 config.yaml::watchdog |
| `ArtifactWatcher` 监听路径 | 已经是 per-project absolute path,不变 |
| `progress.jsonl` 落地 | 仍 `~/.ccteam/progress/<slug>.jsonl`,不分 root(避免迁移痛)|
| `~/projects/meta/`(meta-agent) | 走 `config.yaml::projects[]` 注册,同其他项目 |

---

## 4. F-finding 拆分

| # | 标题 | 改动量 |
|---|---|---|
| F70 | mcp-serve std::process::exit on shutdown | ✅ shipped |
| F71 | web bind default 0.0.0.0 | ✅ shipped |
| F72 | `ccteam init` 三合一 unified install + overwrite strategy | 中(`run_init` 重写,新增 cwd-detect + project scaffold + refresh paths)|
| F73 | `~/.ccteam/config.yaml` 全局 config + projects registry SoT | 中-大(新 `config.rs` 模块,`collect_projects` 改路径,`paths.rs` 加 config-aware fallback)|
| F74 | `ccteam doctor --migrate-v041-to-v042` 一次性迁移 | 小(扫 `~/projects/*` append config + 折 watchdog.yaml)|
| F75 | `ccteam new "<request>"` LLM-auto-slug 路径删除 | 小(删 code + 文档)|

**实现顺序**(依赖关系):
1. **F73** — config.yaml 模块 + projects[] SoT + daemon reads it
2. **F74** — 一次性迁移 doctor 命令(F73 落地后立即可用)
3. **F72** — `ccteam init` unified install(依赖 F73 的 registry append API)
4. **F75** — 删 `ccteam new "<request>"` 路径(在 F72 接管 user-facing 入口后)

---

## 5. 不在本轮的(deferred to V0.4.3+)

- **远程仓库 install**(`ccteam init git@github.com:foo/bar.git`)
- **多个 `projects_root`(数组)**— V0.4.2 是单 canonical root + projects[] 注册表(可在任意路径)
- **Auto-discover by markers**(walk `~`,找 `.ccteam/` 标记自动 register)
- **`ccteam move <slug> <new-path>`** — 移动 already-installed 项目原子化命令
- **config.yaml hot-reload** — daemon 看到 config.yaml 变化不重启就 re-roster(目前依赖 V0.4.1 的 10s rescan tick)

---

## 6. 验收

1. `ccteam init` 在空 cwd / 已有 git repo / 已有 ccteam 项目 都成功,且分别走 create / scaffold / refresh 路径
2. `ccteam init` 在 cwd = ccteam 仓库本身 → fail-loud
3. `ccteam init --force` 覆盖 `.claude/agents/*.md` + `workflow.yaml`;不加 `--force` 默认保留
4. `~/.ccteam/config.yaml::projects[]` 是 SoT;`ccteam ls` / `ccteam status` 读它,daemon roster 读它
5. `ccteam doctor --migrate-v041-to-v042` 在 V0.4.1 home 上跑后 config.yaml 包含所有 `~/projects/*` 条目
6. `daemon` 在 10s rescan tick 内 spawn 新 `ccteam init` 的项目(V0.4.1 hot-reload 沿用)
7. `ccteam abandon <slug>` 默认从 config.yaml 删条目 + 保留磁盘文件;`--remove-files` 同时清 `.ccteam/`+`.claude/`
8. 测试隔离:`CCTEAM_CONFIG_HOME` env override `~/.ccteam/` 让 integration test 走 tempdir

---

## 7. 风险

- **slug uniqueness**:`ccteam init` 必须先 read config.yaml::projects[] check collision,再 reject 冲突
- **多个 ccteam install 抢同一 cwd 的 `.ccteam/`**:cwd 内 `.ccteam/state.json::slug` 必须和 config.yaml entry 一致;不一致 → fail-loud,要求 `--force` 重写
- **abandon 不 kill running session**:架构红线"永不主动 kill 长 session";`ccteam abandon` 只清注册表,daemon 下次 tick 看到 slug 不在 config.yaml 就 stop 监听,运行中的 session 自然完成或孤立(用户决定)
- **config.yaml 损坏**:加载失败 → fail-loud + 提示 `cp config.yaml.bak config.yaml`(写时 atomic + 旧版 .bak 滚一份,同 `state.json` save pattern)
- **测试 isolation**:`~/.ccteam/config.yaml` 是用户级文件,测试要走 tempdir。沿用 `CCTEAM_CONFIG_HOME` env override(已存在)+ 新增 `config.rs` 的 `from_path()` 测试入口
