# ccteam V0.4.6 — 用户手册

> 简明命令清单。主线:**装 ccteam → init 项目 → start daemon → 写 workflow.yaml → 用**。
> V0.4.6 新功能:`ccteam remove` / workflow.yaml 热加载 / `.ccteam/workflow.yaml` / budget cap / `~/.claude/jobs/` GC / graceful shutdown / CLI 瘦身 / cost SoT。
> 深设计 → `docs/v0-4-6/prd.md`。

---

## 0. 前置

```bash
claude --version    # Claude Code 2.x(https://claude.com/claude-code)
tmux -V             # apt install tmux / brew install tmux(Codex adapter 用,Claude bg 不需要)
ccteam --version    # 应该输出 0.4.6
```

## 1. 首次装(一台机一次)

```bash
ccteam doctor --install-all          # skill + MCP + meta-agent + memory-bridge 一锅
# 或:逐项安装
ccteam doctor --install-skill        # ~/.claude/skills/ccteam-{control,team-author,project-creator,creator}/
ccteam doctor --install-mcp          # ~/.claude.json::mcpServers.ccteam(17 工具)
ccteam doctor --install-meta-agent   # ~/projects/meta/ 的常驻 meta-agent
ccteam doctor --install-memory-bridge
```

## 2. V0.4.5 → V0.4.6 升级(已装用户)

```bash
# 1. 拉新 binary
cd <ccteam-repo> && git pull && cargo build --release

# 2. workflow.yaml 从项目根移到 .ccteam/(F83)
ccteam doctor --migrate-workflow-to-ccteam-dir            # dry-run
ccteam doctor --migrate-workflow-to-ccteam-dir --apply    # 真移

# 3. 删现有项目里的 cost-accumulate hook(F91)
ccteam doctor --update-hooks                              # 注:目前默认直接 apply

# 4. ~/.claude/jobs/ 大扫除(F85,默认 retention 7d)
ccteam doctor --gc-claude-jobs                            # dry-run 预览
ccteam doctor --gc-claude-jobs --apply                    # 真删 terminated > 7d 旧的

# 5. 重启 daemon(picks up F82 hot-reload watcher + F86 graceful stop)
ccteam stop && ccteam start
```

V0.4.5 用户不用 V0.4.6 新功能 → 0 break。老 CLI 路径(`ccteam hook ...` / `ccteam spawn` 等)V0.4.6 仍工作 + stderr WARN;V0.5 删。

## 3. 创建 / 接管项目

```bash
# 新项目(从无到有)
mkdir -p ~/projects/myapp && cd ~/projects/myapp && ccteam init

# 已有 git repo 原地装(不动业务代码)
cd ~/code/my-fastapi-app && ccteam init

# thin wrapper(= init --in <projects_root>/<team>-<slug>/)
ccteam new myapp --team dev                # → ~/projects/dev-myapp/

# 已 ccteam'd 项目 refresh(默认 preserve workflow.yaml + agents)
ccteam init
ccteam init --reset-agents                 # 只重写 .claude/agents/*.md
ccteam init --force                        # 全覆盖
```

新建项目结构(V0.4.6 起):
```
<project>/
├── .ccteam/
│   ├── workflow.yaml           ← V0.4.6 F83 起住这,不在项目根
│   ├── state.json
│   ├── config.json
│   └── ...artifact dirs/      ← 你 workflow.yaml 里声明的 watch path
├── .claude/
│   ├── agents/<role>.md        ← 每 agent 一份 prompt(可手写 / 自动生成)
│   └── settings.json
└── <your business code>        ← 不动
```

slug 规则 `[a-z0-9-]+`,长 ≤ 60,首尾不可 `-`。`ccteam new` 自动加 `<team>-` 前缀。

## 4. 写 workflow.yaml(V0.4.6 核心新功能)

`<project>/.ccteam/workflow.yaml`:

```yaml
name: myapp-loop                 # 自由命名
enabled: true                    # V0.4.6 F82:false → daemon 跳过此项目;热加载
budget:                          # V0.4.6 F84:可选 budget cap
  max_cost_usd_per_24h: 5.00     # 24h 累计 cost 超 → 自动 enabled: false
  max_agent_spawns_per_hour: 100 # 1h spawn 超 → 同上(防 self-excitation)

agents:
  explorer:
    executor: claude             # 或 codex
    trigger: watch:.ccteam/issues/       # 看目录,新文件就 spawn
    parallelism: 2

  fixer:
    executor: claude
    trigger: watch:.ccteam/fixes/
    parallelism: 1

  reviewer:
    executor: claude
    trigger: gate                # 等 `mcp__ccteam__trigger_gate` 触发

  master:
    executor: claude
    trigger: manual              # 只有 `ccteam internal spawn` / MCP `spawn_agent` 才起

  schedule_role:
    executor: claude
    trigger: schedule            # V0.4.x 还是简单 startup-once,真 cron 推 V0.5
    interval: 3600
```

**`enabled` + `budget` 热加载** — 改 yaml 存盘,2-5s 内 daemon 检测 + 优雅 reload(`workflow_done reason="reloaded"`)。**无需 `ccteam stop/start`**。

Trigger 速查:
- `manual` — 只有 `mcp__ccteam__spawn_agent` 或 `ccteam internal spawn` 才起
- `gate` — 等 `mcp__ccteam__trigger_gate` 触发(workflow 的"等所有上游搞完才放行"语义)
- `schedule` — 间隔触发(`interval: <秒>`)
- `watch:<relative-path>/` — `<project>/<path>` 下任何新文件 → spawn 一个 session 处理它

`.claude/agents/<role>.md` 写 agent 行为(Anthropic skill-creator 格式)。`ccteam init` 默认生成模板;改后 `ccteam init --reset-agents` 重置。

## 5. 启动 daemon + web UI

```bash
ccteam start                           # orchestrator + web(默认 0.0.0.0:7331,LAN 可达 + token auth)
ccteam start --no-web                  # 只 orchestrator
ccteam start --web-bind 127.0.0.1:7331 # loopback(token auth 自动关)
ccteam start --no-clipboard            # V0.4.6 F88:跳过 clip 探测(CI / headless)

nohup ccteam start > /tmp/ccteam.log 2>&1 < /dev/null & disown   # 后台

ccteam stop                            # V0.4.6 F86 graceful:cancel token + 30s timeout fallback
```

`ccteam start` 输出 banner:URL + token + 自动复制到 clipboard(如 xclip/wl-copy/pbcopy/clip.exe 之一可用)。Token 也常驻 `~/.ccteam/web-token`。

Web UI(V0.4.6 F90 加 4 个新面板):
- **WorkflowView** —— 每个 agent 一张 card,显示 running/queued/cost + 实时 SSE
- **Artifact Queue** —— 每个 watch 目录待处理文件数 + 最老 age
- **Events Timeline** —— `progress.jsonl` tail SSE,颜色编码事件类型
- **Failure Inspector** —— errored agent card 点击 → tail `~/.claude/jobs/<id>/output.log`
- **Cost Sparkline** —— 24h / 7d 趋势

## 6. 查看 / 操作

用户日常 13 命令(`ccteam --help` 列出来都是):

```bash
# 看
ccteam status                              # daemon + projects + recent events + token 一屏
ccteam ls                                  # 项目列表(JSON: --format json)
ccteam show <slug>                         # 单项目完整状态 + recent events + artifacts

# 改
ccteam init / ccteam new                   # 装项目
ccteam start / ccteam stop                 # daemon
ccteam remove <slug>                       # V0.4.6 F81 — 见 §7
ccteam web                                 # 单独起 web(脱离 start)

# 维护
ccteam doctor [<flags>]                    # 见 §9

# 团队 / flex session
# V0.5.0 移除: `ccteam team init/publish/show` 整套子命令删除(team factory 不再维护)。
# 创建新项目 / 自定义 workflow 改走 `ccteam-creator` skill,起 agent team 改走 `/ccteam-team` skill。
ccteam session add/ls/attach/rm <slug>
```

内部 8 命令(给 meta-agent / MCP / hook 用,日常不碰):
```bash
ccteam internal hook progress-append PostToolUse
ccteam internal mcp-serve
ccteam internal spawn <slug> <role> ["<prompt>"]
ccteam internal send <slug> "<msg>" [-r <role>]
ccteam internal peek <slug>                # tmux pane 快照
ccteam internal attach <slug>              # tmux/claude bg attach
ccteam internal progress <slug> [--tail]
ccteam internal resume <slug>
```

## 7. `ccteam remove` — V0.4.6 新功能(F81)

把项目从 `~/.ccteam/config.yaml::projects[]` 摘掉,可选清项目内 `.ccteam/` + `.claude/agents/` + workflow.yaml:

```bash
ccteam remove <slug> --dry-run             # 预览:哪些会动,哪些不动
ccteam remove <slug>                       # 干跑 + 守红线(默认)
ccteam remove <slug> --purge               # 同上 + rm 项目内 ccteam-managed 文件
ccteam remove <slug> --force               # 跳过守红线(用户主动)
```

**守红线**(refuse → 让用户先处理):
- 项目有活的 tmux session `ccteam-<slug>`
- 项目有活的 `claude --bg` job(`~/.claude/jobs/<id>/state.json::state == working` 且 cwd 匹配)
- `progress.jsonl` 显示 open `agent_spawn`(没匹配 `agent_done`)

**永远不删**:
- `<project>/.env`(用户密钥)
- 项目业务代码

## 8. 全局配置 `~/.ccteam/config.yaml`

```yaml
projects_root: ~/projects                  # ccteam new 落地的 base
claude_jobs_retention_days: 7              # V0.4.6 F85:GC 阈值(0 = 禁用)
projects:                                  # 注册表(daemon SoT)
  - slug: dev-myapp
    path: /home/rob/projects/dev-myapp
    team: dev
    installed_at: 2026-05-16T14:00:00Z
watchdog:                                  # 可选
  notify_on_cycle_count: 2
```

env override(测试 / CI):
```bash
CCTEAM_HOME=/tmp/x ccteam ls                     # 换 ~/.ccteam
CCTEAM_PROJECTS_ROOT=/work/repos ccteam new ...
CCTEAM_CLAUDE_JOBS_DIR=/tmp/jobs                 # 测试 GC 时用
CCTEAM_MCP_IDLE_TIMEOUT_SECS=60                  # MCP server idle 退出门槛
```

## 9. doctor 速查

```bash
# 安装
ccteam doctor --install-all                                # skill + MCP + meta-agent + memory-bridge
ccteam doctor --install-skill / --install-mcp / --install-meta-agent / --install-memory-bridge

# 健康检查
ccteam doctor --tool-surface                               # plugin agent 可达性
ccteam doctor --validate-team <name>                       # team.yaml schema 校验
ccteam doctor --screenshot-smoke <slug>                    # 端到端 PNG smoke

# V0.4.6 新维护命令
ccteam doctor --migrate-workflow-to-ccteam-dir [--apply]   # F83:workflow.yaml → .ccteam/
ccteam doctor --update-hooks                               # F91:删 cost-accumulate hook
ccteam doctor --gc-claude-jobs [--apply]                   # F85:清 ~/.claude/jobs/ 残留

# 历史迁移(老用户用得到)
ccteam doctor --migrate-v041-to-v042                       # V0.4.1 → V0.4.2 fold projects 到 config.yaml
ccteam doctor --migrate-recommended-agents                 # V0.1 → V0.2 清 symlink
ccteam doctor --reset-shipped-teams [--force]              # 重置 shipped team templates
```

## 10. MCP 工具(Claude Code session 内自动可用)

`ccteam doctor --install-mcp` 后,任意 Claude Code session 看到 17 个 `mcp__ccteam__*`:

```
ls show peek progress screenshot                         # 读
new pause resume                                          # 项目级
send_to_session inject_decision                           # 路由
spawn_agent stop_agent observe_agents                     # workflow
signal set_parallelism trigger_gate get_artifact_summary  # workflow runtime
```

meta-agent + `ccteam-control` skill 用这套接口自然语言操作。

## 11. 故障排除

```bash
# daemon 不响应
ccteam status                          # heartbeat / projects / token 一屏
pgrep -af "ccteam start" | grep -v bash

# workflow 改了 yaml 不生效
tail -f /tmp/ccteam.log | grep "workflow.yaml change"   # F82 应该 2-5s 内检测到
# 没看到 → daemon 没装 inotify watch,看 daemon 启动 log 里 "workflow_watcher" 有没有报错

# 项目卡住 / cost 漂
ccteam show <slug>                     # cost_24h / cost_active / cost_total + recent events
# V0.4.6 起 cost 实时从 `~/.claude/jobs/<id>/state.json::cost_usd_total` 读
# 老 state.cost_used_usd 字段保留 serde-compat,但**不再写入** — 别看老值

# stale 残留(daemon 异常退出后)
ccteam doctor --gc-claude-jobs --apply   # F85 大扫除
# orchestrator 启动时自动跑一次 GC(retention=7d default)

# remove 卡住 — 见 §7 红线;先 tmux kill-session 或等 bg job 结束,或 --force

# Web token 失效 / 不见
cat ~/.ccteam/web-token                # 当前 token
ccteam status                          # 也输出
rm ~/.ccteam/web-token && ccteam stop && ccteam start   # 重生
```

## 12. 卸载

```bash
# 单项目级
ccteam remove <slug> --purge           # V0.4.6 F81:守红线 + 清 .ccteam/ + .claude/agents/ + workflow.yaml

# 全清(留业务代码)
ccteam stop
rm -rf ~/.ccteam                       # 配置 + 历史 + token
# 各项目里的 .ccteam/ + .claude/agents/ + workflow.yaml 想清就 rm -rf;不影响业务代码 + .env

# binary
rm /usr/local/bin/ccteam               # 或你 cargo install 装的位置
```

---

bug / 想法直接 PR;深问题看 `docs/v0-4-6/prd.md`。
