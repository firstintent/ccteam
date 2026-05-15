# ccteam V0.4.2 — 用户手册

> 简明命令清单。主线:**装 ccteam → init/new 项目 → start 起 daemon → 用**。
> 详细设计 → `docs/v0-4-2/prd.md`。

## 0. 前置

```bash
claude --version    # Claude Code CLI 必装(https://claude.com/claude-code)
tmux -V             # apt install tmux / brew install tmux
ccteam --version    # cargo build --release -p ccteam-cli 取
```

## 1. 首次装 ccteam(一台机一次)

```bash
ccteam doctor --install-all          # skill + MCP + meta-agent 一锅
ccteam init -i                       # 或:逐项 y/n 向导
ccteam init -y                       # 或:一律 yes(脚本/CI)

# 单装
ccteam doctor --install-skill
ccteam doctor --install-mcp
ccteam doctor --install-meta-agent
ccteam doctor --install-memory-bridge
```

## 2. V0.4.1 → V0.4.2 升级

```bash
ccteam doctor --migrate-v041-to-v042 # 把 ~/projects/* + watchdog.yaml fold 进 config.yaml,幂等
cat ~/.ccteam/config.yaml            # 校验 projects[] 写进来了
```

## 3. 创建 / 接管项目(三场景,一个命令)

```bash
# 场景 A:新项目
mkdir ~/projects/myapp && cd ~/projects/myapp && ccteam init
ccteam init --in ~/projects/myapp                  # 等价,不用先 cd
ccteam init --slug myapp --team dev                # 覆盖默认(dir basename / dev)

# 场景 B:已有 git repo 原地装(不动业务代码)
cd ~/code/my-fastapi-app && ccteam init            # slug = "my-fastapi-app"

# 场景 C:已 ccteam'd,refresh
ccteam init                                        # 默认 preserve workflow.yaml + agents
ccteam init --reset-agents                         # 只重写 .claude/agents/*.md
ccteam init --force                                # 全覆盖

# 场景 D:thin wrapper(= init --in <projects_root>/<team>-<slug>/)
ccteam new myapp --team dev                        # → ~/projects/dev-myapp/
ccteam new spike --team research                   # → ~/projects/research-spike/
```

slug 规则 `[a-z0-9-]+`,长 ≤ 60,首尾不可 `-`。不合规 fail-loud:

```bash
ccteam new "做一个 todo cli" --team dev   # ✗ 空格/中文
ccteam new BadSlug --team dev             # ✗ 大写
ccteam new trailing- --team dev           # ✗ 尾随 dash
```

## 4. 启动 daemon + web UI

```bash
ccteam start                                       # orchestrator + web(默认 0.0.0.0:7331)
ccteam start --no-web                              # 只 orchestrator
ccteam start --web-bind 127.0.0.1:7331             # loopback(无 token auth)
ccteam start --web-bind 0.0.0.0:8080               # 换端口

nohup ccteam start > /tmp/ccteam.log 2>&1 < /dev/null & disown   # 后台

ccteam stop                                        # 干净 SIGTERM
```

banner 含 URL + token。token 也常驻 `~/.ccteam/web-token`。

## 5. 查看 / 操作

```bash
ccteam status                              # daemon + projects + 事件 + token 一屏
ccteam ls / ls --format json               # 项目列表
ccteam show / show <slug> / show <slug> --format json
ccteam progress <slug> [--tail]            # 事件流
ccteam peek <slug>                         # tmux pane 快照
ccteam attach <slug>                       # tmux/bg attach,自动判

ccteam pause <slug> / resume <slug>
ccteam send <slug> "<msg>"                 # 写 inbox,触发 spawn
ccteam send <slug> -r reviewer "<msg>"     # 指定 target role
ccteam spawn <slug> <role> ["<prompt>"]    # 显式 spawn

# flex team only
ccteam session add <slug> --harness claude|codex
ccteam session ls <slug>
ccteam session attach <slug> <sid>
ccteam session rm <slug> <sid>
```

## 6. 全局配置 `~/.ccteam/config.yaml`

```yaml
projects_root: ~/projects             # ccteam new 落地的 base
projects:                              # 注册表(daemon SoT)
  - slug: dev-myapp
    path: /home/rob/projects/dev-myapp
    team: dev
    installed_at: 2026-05-15T14:00:00Z
watchdog:                              # 可选,V0.4.1 老 watchdog.yaml fold 进来
  notify_on_cycle_count: 2
```

env override(测试):

```bash
CCTEAM_HOME=/tmp/x ccteam ls                     # 换 ~/.ccteam
CCTEAM_PROJECTS_ROOT=/work/repos ccteam new ...  # 换 projects_root
CCTEAM_MCP_IDLE_TIMEOUT_SECS=60                  # mcp-serve idle 退出门槛
```

## 7. MCP 工具(在 Claude Code session 内自动可用)

`ccteam doctor --install-mcp` 后,任意 Claude Code session 看到 17 个 `mcp__ccteam__*`:

```
ls show peek progress screenshot
new pause resume
send_to_session inject_decision
spawn_agent stop_agent observe_agents
signal set_parallelism trigger_gate get_artifact_summary
```

## 8. doctor 速查

```bash
ccteam doctor                                # 列模式
ccteam doctor --tool-surface                 # 检查工具可达
ccteam doctor --install-all                  # = --install-skill + --install-mcp + --install-meta-agent
ccteam doctor --reset-shipped-teams [--force]
ccteam doctor --validate-team <name>
ccteam doctor --migrate-recommended-agents   # V0.1 → V0.2 清 symlink
ccteam doctor --migrate-v041-to-v042         # V0.4.1 → V0.4.2 一次性迁移
ccteam doctor --screenshot-smoke <slug>      # 端到端 PNG smoke
ccteam doctor --dry-run ...                  # 干跑 modifier
```

## 9. 排查

```bash
# daemon 健康
ccteam status                                # 看 heartbeat / projects 数 / token
pgrep -af "ccteam start|ccteam mcp-serve" | grep -v bash

# mcp-serve 残留(V0.4.2 后会自动孤儿退出 + idle exit)
# 老 binary 残留单独 kill <pid>(精确 pid,绝不 pkill -f 跨 ssh)

# 拒装路径
cd ~ && ccteam init                          # ✗ refusing — $HOME
ccteam init --in /                           # ✗ refusing — fs root
ccteam init --in ~ --force                   # 真要装就 --force

# slug 冲突(同 slug 不同路径)
ccteam init --slug myapp --in /tmp/other     # ✗ already registered at ...
ccteam init --slug myapp --in /tmp/other --force   # 强制重指

# web token
cat ~/.ccteam/web-token                      # 或 `ccteam status` 输出

# 全清(不删用户项目)
ccteam stop
rm -f ~/.ccteam/state/{orchestrator.pid,heartbeat}
```

## 10. 卸载

```bash
ccteam stop
rm -rf ~/.ccteam                             # 配置 + 历史一锅清
# 项目目录里的 .ccteam/ + .claude/ 想清就 rm -rf,不影响业务代码
```

---

bug 直接 PR 改;深问题看 `docs/v0-4-2/prd.md`。
