# ccteam 使用指南

一份命令为主的端到端用户指南:install → init → start → 接入 IM → 日常用 → 运维。
直接照着代码块敲。所有命令都对照当前 CLI 校验过。

核心模型:**一个 chat 就是一台终端**。三个对象——

- **chat**:Telegram 私聊或群聊。每个 chat 有自己的当前 project、当前 session、session 列表,互相隔离。
- **project**:本地一个已 `ccteam init` 的目录,用 slug 标识。
- **session**:一个可继续上下文的 agent 会话,属于某个 chat + 某个 project。

两类命令别混:**shell 里的 `ccteam <子命令>`**(运维 + 安装) vs **IM 里的网关命令 `/pair /new /use /cd /sessions /projects`、路由前缀 `@<handle>`、管理 `@ccteam`**(日常对话)。

---

## 1. 安装

```bash
# 装 ccteam CLI(GH Releases 预编译;linux + macOS arm/x64,Windows 走 WSL2)
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version
```

```bash
# 提示 ~/.local/bin 不在 PATH 时,加进 shell 配置后重开终端
export PATH="$HOME/.local/bin:$PATH"
```

```bash
# fallback:从源码装(二进制名仍是 ccteam)
cargo install --git https://github.com/firstintent/ccteam ccteam-cli
```

ccteam 不打包 Claude / Codex,调用你机器上的真实 CLI。确认装好且已登录:

```bash
claude --version          # 需要时按提示登录
codex --version           # 用 Codex bot 才需要
```

在 Claude Code 会话里装插件(`/plugin` 是 Claude Code 内建命令,给 agent 提供 `mcp__ccteam__*` 工具):

```text
/plugin marketplace add https://github.com/firstintent/ccteam
/plugin install ccteam
```

```bash
# Codex 用户装 Codex 插件
codex plugin marketplace add firstintent/ccteam
```

```bash
# 体检 + MCP 表面自检(应显示 active 27 / stubs 0)
ccteam doctor
ccteam doctor --verify-mcp
```

```bash
# 装插件后 Claude 看不到 MCP 工具时,显式注册再重载会话
ccteam doctor --install-mcp
```

---

## 2. 初始化项目

一个 chat 可同时管多个项目。在每个项目目录跑一次 `init`:

```bash
cd ~/projects/demo-app
ccteam init --slug demo-app
```

```bash
cd ~/projects/demo-api
ccteam init --slug demo-api
```

`ccteam init` 写入 `.ccteam/{agents,skills,state.json}`、`.claude/agents`,以及一份自动管理的 `<project>/CLAUDE.md`。业务代码 / `.git/` / `.env` 永远保留;重跑安全。

```bash
ccteam init --in /path/to/repo --slug myproj    # 在别处初始化
ccteam init --force                             # 覆盖 ccteam 生成物(在源码目录自举时也用它)
ccteam ls                                        # 列已知项目
```

---

## 3. 接入 IM(Telegram bot)

在 Telegram 找 `@BotFather` → `/newbot` 拿 token。给 bot 发一条消息(群里则把 bot 拉进去发一条),再取 chat id:

```bash
BOT_TOKEN='123456:replace_me'
curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getUpdates"   # 在输出找 message.chat.id
```

手写本机凭证(私聊 chat id 通常正数,群聊可能负数):

```bash
mkdir -p ~/.ccteam/im
chmod 700 ~/.ccteam ~/.ccteam/im
$EDITOR ~/.ccteam/im/credentials.json
chmod 600 ~/.ccteam/im/credentials.json
```

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

`allowed_chat_ids` 是安全边界:只有列出的 chat 能触达 daemon,**生产不留空**。改完凭证必须重启 daemon 才生效。

---

## 4. 启动 gateway daemon

```bash
ccteam start > /tmp/ccteam.log 2>&1 &     # 一个进程:IM gateway + MCP socket + web UI
```

`ccteam start`(无 slug)启动常驻网关,同进程提供:IM gateway(Telegram 长轮询 + 出站)、MCP socket(`~/.ccteam/run/mcp.sock`)、Web UI(默认 `0.0.0.0:7331`)、hook sink。

```bash
ccteam start --web-bind 127.0.0.1:7331    # 只绑环回(此时 web 不需 token)
ccteam start --no-web                      # 只要网关,不起 web
ccteam start --no-imd                      # 只要 web,不起 IM 网关
```

```bash
# 重启(只停 daemon,不杀 tmux session;重启后按 session id 自动接回)
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

> `--web-bind` 是 `ccteam start` 的 web 地址参数;独立的 `ccteam web` 子命令用 `--bind`。两者别搞混。

---

## 5. 日常使用(IM 网关命令)

配对当前 chat(`<code>` 任意,如 `phone`;回 `paired <code>`):

```text
/pair phone
```

切项目、建 session(`/new <vendor> <handle>`,vendor = `claude` | `codex`):

```text
/cd demo-app
/new claude reviewer
/cd demo-api
/new codex api
```

切换 / 查看:

```text
/use s1            切到 session s1
/cd demo-api       当前 chat 切到 demo-api;活动 session 跟着切(无则下条消息在那新建)
/sessions          列当前 chat 的 session
/projects          列 daemon 已知项目(= 已 init 且 daemon 已加载)
```

发消息 + slash 透传(`@handle` 决定路由并设为当前 session;不带 `@` 时发给当前 session):

```text
@reviewer 看一下这个项目的 README,给我三条风险
@api /review       Codex 原生 RPC(review/start)
@api /compact      Codex 原生 RPC(thread/compact/start)
@reviewer /clear   Claude TUI slash 透传
```

> Claude session:任意 `/x` 都按字面发给 TUI。Codex session:仅 `/compact` `/review` 走 app-server RPC,其他 `/x` 会被当普通文本。
> gateway 先回 `submitted <session> turn <id>`,随后把 assistant / error 事件经同一条 outbound ledger 发回 IM。

给 bot 设定固定行为走官方机制(**不靠注入**):Claude 读项目根 `CLAUDE.md`(`init` 已生成),Codex 读项目根 `AGENTS.md`(需手建);全局指令放 `~/.claude/CLAUDE.md`。

```bash
$EDITOR ~/projects/demo-app/CLAUDE.md     # Claude bot 下个 turn 生效
$EDITOR ~/projects/demo-api/AGENTS.md     # Codex bot 读取
```

---

## 6. Web 控制台

```bash
# token 落在文件里;ccteam start 已尝试自动复制到剪贴板(--no-clipboard 跳过)
cat ~/.ccteam/web-token
```

- 本机环回:`ccteam start --web-bind 127.0.0.1:7331` → 浏览器开 `http://127.0.0.1:7331`(无需 token)。
- LAN / 非环回 bind(默认 `0.0.0.0:7331`):自动开 token 鉴权,用上面 `~/.ccteam/web-token` 的值。

```bash
ccteam web --bind 127.0.0.1:7331          # 单独起 web(不带网关)
```

---

## 7. 运维

```bash
ccteam status                  # daemon 心跳 + 每个项目 OK/warn/STUCK + 活跃 session
ccteam sessions                # 列活的网关 chat-mode session,并标出 orphan
ccteam doctor                  # 安装 / 依赖体检
ccteam doctor --verify-mcp     # MCP 表面验收(active 27 / stubs 0)
ccteam stop                    # 优雅停 daemon(保留 tmux session)
```

成本(按 vendor 记账):

```bash
ccteam doctor --check-cost-orphan          # ledger 对账(shell 侧)
```

```text
@ccteam cost today             IM 侧 24h 成本汇总(也可 @ccteam cost <slug>)
@ccteam status                 daemon + bot 状态
@ccteam list                   列已注册 bot
@ccteam pause <slug>           暂停 / 恢复某项目自动派工(永不 kill 长 session)
@ccteam resume <slug>
@ccteam stop <slug>            停某项目的 bot;@ccteam stop everything 停全部(危险,需二次确认)
```

状态文件速查:

```bash
tail -120 /tmp/ccteam.log                  # daemon 日志
tail -80 ~/.ccteam/imd/outbound.jsonl      # outbound ledger(重启后重放 queued/failed)
cat ~/.ccteam/imd/gateway-state.json       # gateway session state
cat <project>/.ccteam/state.json           # 项目状态
```

环境变量(隔离 / 覆盖路径):

```bash
CCTEAM_HOME=...                            # 隔离 daemon state / ledger / config(默认 ~/.ccteam)
CCTEAM_PROJECTS_ROOT=...                    # 默认项目根(默认 ~/projects)
CCTEAM_CLAUDE_BIN=... CCTEAM_CODEX_BIN=...  # 覆盖 vendor CLI 路径
CCTEAM_CODEX_APP_SERVER_TRANSPORT=stdio     # Codex 走 codex app-server --listen stdio://
```

---

## 8. 卡住时(最常见 5 条)

```bash
# 先跑这三条,八成能定位
ccteam doctor
ccteam status
tail -120 /tmp/ccteam.log
```

1. **`ccteam: command not found`** — `~/.local/bin` 不在 PATH:`export PATH="$HOME/.local/bin:$PATH"`。
2. **Telegram 不回 / `drop msg from non-allowed chat`** — chat id 不在 allowlist,或改了凭证没重启:编辑 `~/.ccteam/im/credentials.json` 的 `allowed_chat_ids` → `ccteam stop && ccteam start`。
3. **IM 收到 `gateway error: ...`(pane 死 / socket 断)** — `ccteam stop && ccteam start`,再发同一 `@handle`;仍失败用 `/new claude reviewer2` 开新 session。
4. **`gateway error: turn timed out ...`** — 等一会重试;长上下文先 `@bot /compact`;反复超时就新建 session。
5. **`/new` 报 `unknown project`** — 项目没 init 或 daemon 没加载:`cd <repo> && ccteam init --slug <s>` → `ccteam stop && ccteam start` → IM 里 `/projects` 确认 → `/cd <s>`。

> IM 路径里的 Claude session 使用 `--dangerously-skip-permissions`(无批准门,YOLO 模式)——只把 bot 暴露给可信 chat,bot token 不进 git。
