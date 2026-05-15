# PRD V0.4.2 — Adopt-an-existing-repo + custom projects root + hotfix sweep

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
| **F72** | **`ccteam adopt` — 在已有仓库目录上接管 ccteam(原地,不动文件)** | **新功能** | 待 review/实现 |
| **F73** | **多 root projects 注册表 — `~/projects/` 之外的路径也能被 daemon 监控** | **新功能** | 待 review/实现 |

F70/F71 已经在 main 上,本 PRD 留作 ship 记录。F72/F73 是本轮的真正需求。

---

## 1. F72 — Adopt 已有仓库目录

### 1.1 用户场景

```bash
# 用户已有项目
cd ~/code/my-fastapi-app
git log -1   # 已有几百次 commit,代码已 production

# 现在想让 ccteam 帮忙跑 review / 加测试 / 重构等
ccteam adopt               # 在 cwd 创建 .ccteam/state.json + .claude/agents/*
# OR
ccteam adopt --slug my-app --team dev
```

**核心需求**:不移动用户代码,不重命名目录,不要求住进 `~/projects/`。在仓库的根目录 **原地** 放 ccteam 的管理文件:

```
~/code/my-fastapi-app/        ← 用户已有仓库
├── .git/
├── src/                      ← 用户已有代码
├── README.md                 ← 用户已有文件
├── .ccteam/                  ← ccteam 新增(gitignore 友好)
│   ├── state.json
│   ├── workflow.yaml         ← scaffold 一份示例
│   ├── inbox/
│   └── outbox/
└── .claude/                  ← ccteam 新增
    ├── settings.json
    └── agents/
        ├── reviewer.md
        └── implementer.md
```

### 1.2 今天的限制

- `ccteam new` 强制 `~/projects/<team>-<slug>/`(F22)
- `bootstrap_project` 调 `pick_unused_slug` 然后 `paths.project_dir(slug)` —
  路径完全由 `paths.projects_root` 决定
- daemon `collect_projects` 只 walk `paths.projects_root`
- 没有任何 "我已经有一个 dir,把它注册进来" 的 API

### 1.3 设计选择

**选项 A**:把仓库 symlink 到 `~/projects/<team>-<slug>/`
- 优点:零改动 daemon / orchestrator
- 缺点:`.ccteam/` 实际上写在符号链接 target(用户仓库根),用户 git status 看到一堆陌生文件(symlink chain 是隐式的);Windows / WSL 子系统跨 mount symlink 容易翻车
- ⛔ 弃

**选项 B**(推荐):多 root 注册表
- `~/.ccteam/projects.yaml` 维护一个 `{slug → absolute_path}` map
- daemon `collect_projects` 既走 `~/projects/` 又走注册表中的路径
- adopt 命令:在 cwd 写 `.ccteam/` skeleton + 在注册表 append 一行
- 优点:零 symlink / 零跨文件系统问题;一份代码同时承载新建项目和已有项目
- 缺点:注册表是新的 SoT(per CLAUDE.md 红线"progress.jsonl 是 SoT" — 但 projects.yaml 是 *roster*,不是事件流,不冲突)

**选项 C**:不动文件系统,在 `~/projects/<slug>/` 写一个 `.target` 文件指向真实路径
- 优点:沿用现有 `~/projects/` walk
- 缺点:额外 indirection;每个 read 要先读 `.target` 再去真实路径,API 改动比选项 B 多

→ **采用选项 B**。

### 1.4 接口

```bash
# 在仓库内 adopt(slug 默认用 cwd basename)
cd ~/code/my-fastapi-app
ccteam adopt
ccteam adopt --slug fastapi-app             # slug override
ccteam adopt --team dev                     # team override(默认 dev)
ccteam adopt --workflow-template review     # 用预置 workflow.yaml 模板填初稿
ccteam adopt --yes                          # 跳过 y/n 确认(同 init -y)

# 列已注册的 external project
ccteam ls                                   # 同时列 ~/projects/* 和注册表
ccteam ls --external-only                   # 只列注册表里的

# 注销(不删用户文件,只清注册表 + 可选清 .ccteam/)
ccteam abandon <slug>                       # 默认 --keep-files
ccteam abandon <slug> --remove-files        # 把 .ccteam/ + .claude/ 也清掉
```

### 1.5 注册表格式 `~/.ccteam/projects.yaml`

```yaml
# ccteam external project registry (V0.4.2 F72).
# Slugs MUST be unique across ~/projects/ canonical layout AND this
# file. Adopting a slug that collides with a ~/projects/ entry is a
# fail-loud error.
adopted:
  - slug: fastapi-app
    path: /home/rob/code/my-fastapi-app
    team: dev
    adopted_at: 2026-05-15T14:23:11Z
  - slug: react-spa
    path: /home/rob/code/react-frontend
    team: design
    adopted_at: 2026-05-15T16:01:00Z
```

注:`path` 是绝对路径。如果用户 mv 了目录,下次 `ccteam ls` 应该 warn "path no longer exists" 并提示用 `ccteam abandon` 或 手动 patch yaml。**ccteam 不自动追 dir moves**。

### 1.6 Daemon 改动

`collect_projects(paths)` 现在两步:
1. walk `paths.projects_root`(已有)
2. read `~/.ccteam/projects.yaml`,对每个 `adopted` 条目用 `ProjectSummary` 同样的 schema 加进 list

daemon 的 P1 hot-reload 机制(V0.4.1 `spawn_new_rostered_projects`)继续工作 — 新 adopted slug 在下一 rescan tick(10s)自动被 spawn。

### 1.7 边角情况

- **slug collision with ~/projects/**:同名时 fail-loud,要求用户 `--slug other-name` rename
- **adopt 一个已经有 `.ccteam/` 的目录**:要求 `--force` 或拒绝
- **adopt 一个非 git 仓库**:warn 但允许(ccteam 不强制要求 git)
- **adopt 在 ccteam 仓库本身**:CLAUDE.md §六 已有警告"不要给 ccteam 自己加 ccteam 风格的 hook"。adopt 在 cwd == ccteam 仓库根时 fail-loud(black-list `Cargo.toml` + `crates/ccteam-cli` 同时存在)
- **abandon 不在 yaml 里的 slug**:fail-loud(不是 fail-silent)

---

## 2. F73 — Custom projects root

### 2.1 用户场景

> "我的代码都放在 `/work/repos/`,不在 `~/projects/`。能不能让 ccteam 默认 root 改成 /work/repos/?"

或:

> "我有两个磁盘,SSD `/ssd/projects/` 跑临时 spike,大盘 `/data/projects/` 跑长期项目。两个我都想要 ccteam 管。"

### 2.2 今天的限制

`paths.projects_root` 由 `CcteamPaths::from_env()` 算出,目前是:
- 如果 `$CCTEAM_PROJECTS_ROOT` 设置,用它
- 否则 `$HOME/projects`

CCTEAM_PROJECTS_ROOT env 已经存在(env override),但:
- 文档没说明
- 单 root 不是多 root
- 用户重启 shell 后 env 丢失,daemon / cli 看到的 root 可能不一致

### 2.3 设计

**两件事不要混淆**:
- **canonical projects root**(单值)— ccteam 新建项目落地的地方,`ccteam new` 的 base dir
- **external adopted projects**(多值,F72)— 已有仓库的注册表

### 2.4 接口

#### `ccteam init` 接受 `--projects-root <path>`

```bash
ccteam init --yes --projects-root /work/repos
# 写 ~/.ccteam/config.yaml: projects_root: /work/repos
```

#### `~/.ccteam/config.yaml`(新文件)

```yaml
# Global ccteam config — written by `ccteam init`, read by every
# CLI / daemon / MCP invocation that needs `CcteamPaths`.
# Env override (`$CCTEAM_PROJECTS_ROOT`) still wins for hermetic
# test harnesses; otherwise this file is the SoT.
projects_root: /work/repos        # default if missing: ~/projects
```

#### `CcteamPaths::from_env` 优先级

1. `$CCTEAM_PROJECTS_ROOT` env(测试 + ad-hoc override)
2. `~/.ccteam/config.yaml::projects_root`(install-time setting)
3. `$HOME/projects`(default)

(1) 不变,(2) 是新的,(3) 不变。

#### `ccteam status` 显示 projects_root

```
ccteam status
  daemon:        healthy (heartbeat 3s ago)
  projects_root: /work/repos              ← V0.4.2 新增行
  rostered:      4 projects (2 canonical + 2 adopted)
  …
```

### 2.5 迁移

V0.4.1 用户(用默认 `~/projects/`)升级 V0.4.2:
- 不跑 `ccteam init` → `~/.ccteam/config.yaml` 不存在 → 继续 fallback `~/projects/`,**零变化**
- 跑 `ccteam init --projects-root /new/path` → 写 config,**新建项目落到新 path**,但 `~/projects/` 里的已有项目仍然由 daemon 加载(`collect_projects` 走的是 paths.projects_root,但不会自动 migrate 旧 path 下的项目)

**注**:`init --projects-root` **不会** 自动 mv 旧 `~/projects/<slug>/`。用户要切 root,要么:
- 接受新 root 不包含旧项目(旧的还在 `~/projects/`,daemon 看不到了)
- 手动 mv `~/projects/*` 到新 root,daemon 下次 tick 重 roster
- 用 F72 的 `ccteam adopt /old/path/<slug>` 把旧的逐个注册进来(slug 保留)

V0.4.2 doctor 命令应该 detect 这种漂移 + 提示:

```bash
ccteam doctor --check-projects-root
# WARN: projects_root is /work/repos but ~/projects/ contains 3
# unreachable projects: dev-foo, dev-bar, dev-baz
# Suggest:
#   - mv ~/projects/* /work/repos/   # 物理迁移
#   - ccteam adopt ~/projects/<slug> # 单独 adopt
#   - rm -rf ~/projects/{slug}       # 弃用
```

---

## 3. 交叉影响

| 改动 | 影响位置 |
|---|---|
| 新 `ccteam adopt` 子命令 | `crates/ccteam-cli/src/main.rs::Command`,`crates/ccteam-cli/src/commands.rs::run_adopt` |
| `~/.ccteam/projects.yaml` 注册表 | 新模块 `crates/ccteam-core/src/registry.rs`,被 `queries::collect_projects` 调用 |
| `~/.ccteam/config.yaml` 全局 config | 新模块 `crates/ccteam-core/src/config.rs`,被 `CcteamPaths::from_env` 调用 |
| `ProjectSummary::path` 字段 | `queries::collect_projects` 返回时填实际路径(不再都是 `paths.project_dir(slug)`)|
| `actions::*` 路径解析 | 所有 `paths.project_*(slug)` 调用要走 "lookup slug → real path"(adopted 项目走注册表,canonical 走 projects_root)|
| `ArtifactWatcher` 监听路径 | 已经是 per-project absolute path,不变 |
| `progress.jsonl` 落地 | 仍然 `~/.ccteam/progress/<slug>.jsonl`(per-slug 全局,不分 root,避免迁移痛)|
| `~/projects/meta/`(meta-agent) | meta-agent 一律走 canonical projects_root,不参与 F72 注册表(meta 是 ccteam 自己生成的,不应该被 "adopt") |

---

## 4. F-finding 拆分

| # | 标题 | 改动 |
|---|---|---|
| F70 | mcp-serve std::process::exit on shutdown | ✅ shipped |
| F71 | web bind default 0.0.0.0 | ✅ shipped |
| F72 | ccteam adopt + projects.yaml registry | new — see §1 |
| F73 | ~/.ccteam/config.yaml + projects_root override | new — see §2 |
| F74 | `ccteam doctor --check-projects-root` drift detector | new — see §2.5 |
| F75 | `ccteam abandon` 反向操作 | new — see §1.4 |

**F-finding 依赖**:F72 依赖 F73(adopt 项目要 lookup 真实路径,需要 path resolution helper)。建议实现顺序:F73 → F72 → F74 → F75。

---

## 5. 不在本轮的(deferred to V0.4.3+)

- **远程仓库 adopt**(`ccteam adopt git@github.com:foo/bar.git`)— V0.4.2 只支持本地路径
- **多个 `projects_root`(数组)**— V0.4.2 仍是单 canonical root + 注册表;真要多 disk 就用 F72 adopt 单个项目
- **Auto-discover by markers**(walk `~`,找有 `.ccteam/` 标记的 dir 自动 register)— 太魔法,先不做
- **Git submodule support**— adopt 一个 submodule path 时的特殊处理。先按普通 dir 处理,后续看用户需求
- **`ccteam move <slug> <new-path>`**— 移动一个已 adopted 项目。先用 abandon + adopt 组合,后续再看是否值得做原子化命令

---

## 6. 验收

1. 在已有非 ccteam 仓库 cwd 执行 `ccteam adopt` 后,该仓库出现在 `ccteam ls` + `ccteam status` 输出里
2. daemon 在 10s rescan tick 内 spawn 新 adopted 项目的 event loop(F73 P1 已实现的 hot-reload)
3. `~/.ccteam/projects.yaml` 是注册表 SoT;手动 edit yaml 后下一次 `ccteam ls` 反映改动
4. `ccteam init --projects-root /new/path` 修改 canonical root,旧项目不被自动 mv
5. `ccteam doctor --check-projects-root` 检测漂移并打印 actionable 提示
6. `ccteam abandon <slug>` 默认保留文件,加 `--remove-files` 清 `.ccteam/`+`.claude/`
7. 测试:adopt 一个已经在 canonical root 的 slug → fail-loud;adopt 同一路径两次 → 第二次要 `--force`

---

## 7. 风险

- **slug uniqueness across 两种 root**:adopt 必须先 scan canonical root 的 slug 集合 + 注册表的 slug 集合,再 reject 冲突
- **删除一个 adopt 项目的时序**:abandon 命令是不是先 kill running session 再清注册表?目前架构红线"永不主动 kill 长 session" — abandon **不 kill**,只清注册表;daemon 下次 tick 发现 path 不再注册就 stop 监听
- **测试 isolation**:`~/.ccteam/projects.yaml` 是用户级文件,测试要走 tempdir。`CcteamPaths` 已经支持 `CCTEAM_CONFIG_HOME` env override,F72/F73 沿用同样模式
