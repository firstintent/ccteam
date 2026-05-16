# V0.4.4 — hotfix: 任意路径项目下的 hooks / daemon / MCP SoT 化

> 单 finding hotfix。Ship 后归档。

## 1 个 finding

- **F77** `session_context_from_cwd` walk-up + `paths.project_dir(slug)` 走 config.yaml registry — V0.4.2 引入 `~/.ccteam/config.yaml::projects[]` 任意路径项目,但 hooks / daemon / MCP 三处的 slug → 路径解析仍 hardcoded `paths.projects_root.join(slug)`,导致非 `~/projects/` 路径下的项目 hook 崩溃 + daemon 无法 spawn + MCP `peek` 找不到 tmux session。

## 修复

- `session_context_from_cwd(cwd, paths)` 改 walk-up:从 cwd 往上找最近的 `.ccteam/state.json` 而非匹配 prefix
- `paths.project_dir(slug)` 改 lazy-consult config.yaml:先查 registry,fallback `projects_root.join(slug)`(V0.4.1 用户兼容)
- 202 个调用站点零改动 — 改的是 `CcteamPaths` 内部解析逻辑,所有 caller 自动走新路径

## 触发

V0.4.2 + V0.4.3 落地后用户反馈:`cd ~/code/my-fastapi-app && ccteam init` 装好后,daemon spawn explorer 时 cwd 错(走 `~/projects/my-fastapi-app` 不存在的路径) → claude bg job 启动即 fail。

## 与 V0.4.2 / V0.4.3 / V0.4.5 的关系

- V0.4.2(F72-F75)引入任意路径项目 — 但只在 `ccteam init` / `ccteam new` 入口处理路径,内部 SoT 没跟上
- V0.4.3(F76)slug grammar — 独立问题
- V0.4.5(F78)后续修了 watcher 项目相对路径(另一个 hardcoded 假设)

## 详情

代码改 `crates/ccteam-core/src/paths.rs`(`session_context_from_cwd` + `project_dir`)+ 配套测试。Commit `3403a57`。无 PRD/dev-plan — 单点 hotfix。
