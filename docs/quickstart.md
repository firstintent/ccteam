# Quickstart(全命令版,不依赖 skill)

目标:5-10 分钟从空机器跑到 Telegram 里第一条往返。**全程只用确定性命令** —— shell 里的 `ccteam` CLI + Telegram 里的网关命令(`/pair` `/new` `/cd` …)+ 手动配置文件;**不调用任何 `/ccteam*` skill**。

> **命令分两类,别混:**
> - **确定性(本指南只用这些)**:shell 里的 `ccteam <子命令>`;Telegram 里的网关命令 `/pair` `/new` `/cd` `/use` `/sessions` `/projects`、路由前缀 `@<handle>`、管理 `@ccteam`。这些由 ccteam 二进制 / 网关路由器**直接处理**,行为固定可复现。
> - **LLM skill(本指南一概不碰)**:`/ccteam`、`/ccteam-creator`、`/ccteam-im-setup` 等。它们由模型解释执行,当前**有不确定性**。凡是需要它们的活,本指南都给了等价的确定性做法。

## 1. 安装 CLI 和插件

先装 `ccteam` CLI:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version
```

如果脚本提示 `~/.local/bin` 不在 `PATH`,把它打印的 `export PATH=...` 加到 shell 配置后重开终端。

在 Claude Code 里装插件(`/plugin` 是 Claude Code 的内建命令,确定性,**不是** ccteam skill):

```bash
claude
```

Claude 会话内执行:

```text
/plugin marketplace add https://github.com/firstintent/ccteam
/plugin install ccteam
```

Codex 用户也装 Codex 插件:

```bash
codex plugin marketplace add firstintent/ccteam
```

> 插件的作用是给 agent 提供 ccteam 的 MCP 工具(`mcp__ccteam__*`)。装上即可,**你不需要、也不应该**为了部署去调用任何 `/ccteam*` skill。

最后跑一次体检:

```bash
ccteam doctor
ccteam doctor --verify-mcp
```

`--verify-mcp` 应显示 `27 active, 0 stubs`。

## 2. 登录 Claude 和 Codex

`ccteam` 不打包 Claude / Codex 二进制,它调用你机器上的真实 CLI。

```bash
claude --version
codex --version
```

如果任一命令要求登录,按对应 CLI 的提示完成,然后重跑 `ccteam doctor`。

## 3. 初始化项目

一个 chat 可以同时切多个项目。下面用两个本地目录演示:

```bash
mkdir -p ~/projects/demo-app ~/projects/demo-api

cd ~/projects/demo-app
ccteam init --slug demo-app

cd ~/projects/demo-api
ccteam init --slug demo-api
```

`ccteam init` 会在项目里写入 `.ccteam/{agents,skills,state.json}`、`.claude/agents`,以及一份自动管理的 `<project>/CLAUDE.md`(第 7 节用它给 bot 设定行为)。重跑安全;要覆盖 ccteam 生成物加 `--force`。业务代码 / `.git/` / `.env` 永远保留。

## 4. 配 Telegram bot(手动写凭证)

在 Telegram 找 `@BotFather` 创建 bot 并复制 token。给你的 bot 发一条消息(或把 bot 加进目标群发一条),用下面命令拿 chat id:

```bash
BOT_TOKEN='123456:replace_me'
curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getUpdates"
```

在输出里找到目标 `chat.id`,**手写**本机凭证文件(这一步等价于 `/ccteam-im-setup` skill 的产物,但手写完全确定,本指南推荐):

```bash
mkdir -p ~/.ccteam/im
chmod 700 ~/.ccteam ~/.ccteam/im
cat > ~/.ccteam/im/credentials.json <<'JSON'
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
JSON
chmod 600 ~/.ccteam/im/credentials.json
```

`allowed_chat_ids` 是安全边界:只有这些 chat 能触达 daemon。生产环境不要留空。

## 5. 启动 gateway daemon

```bash
ccteam start > /tmp/ccteam.log 2>&1 &
```

这个进程同时提供:

- IM gateway:Telegram 入站/出站长连接
- MCP socket:`~/.ccteam/run/mcp.sock`
- Web UI:默认 bind `0.0.0.0:7331`(LAN 可达,自动开 token 鉴权);本机访问 `http://127.0.0.1:7331`。只想本地用就 `ccteam start --bind 127.0.0.1:7331`,只想要网关不要 web 就 `--no-web`。

看状态 / 停机:

```bash
ccteam status
tail -40 /tmp/ccteam.log
ccteam stop
```

`ccteam stop` 只停 daemon,不杀 Claude tmux session。下次 `ccteam start` 按 session id 接回。

## 6. 在 Telegram 里跑第一条消息(全是网关命令)

给 bot 发送(`/pair` 等都是网关路由器直接处理的确定性命令,不是 skill):

```text
/pair phone
```

预期回复:`paired phone`。然后创建两个 session:

```text
/cd demo-app
/new claude reviewer

/cd demo-api
/new codex api
```

看列表:

```text
/sessions
/projects
```

你现在有同一个 chat 里的两个 harness:

- `@reviewer`:Claude Code tmux TUI session
- `@api`:Codex app-server session

发消息:

```text
@reviewer 看一下这个项目的 README,给我三条风险
@api /review
@api /compact
@reviewer /clear
```

`@<handle>` 决定路由;不带 `@handle` 时消息发给当前 session。`@reviewer /clear`、`@api /compact` 这类是把 slash **透传**给对应 vendor(Claude 走 TUI send-keys,Codex 走原生 RPC `thread/compact/start` / `review/start`)。

## 7. 给 bot 设定行为(确定性方式:项目 CLAUDE.md,不走 creator skill)

关键认知:`/new claude reviewer` 起的是一个**普通 `claude` 会话**,cwd 在该项目目录;`reviewer` 只是**路由 handle + 会话名**,本身不是行为人格。要让 bot 有固定行为,用 Claude Code / Codex 的官方机制 —— **项目根的 `CLAUDE.md`**,bot 启动时自动读取,无需任何 skill:

```bash
# ccteam init 已生成 demo-app/CLAUDE.md,直接编辑它(或追加)即可:
$EDITOR ~/projects/demo-app/CLAUDE.md
# 例如写:你是这个项目的代码 reviewer。每次先给三条最高风险,再给可执行下一步。中文。
```

下次该项目的 bot 起 turn 时即生效。全局指令放 `~/.claude/CLAUDE.md`。

Codex bot 读的是 `AGENTS.md`(不是 `CLAUDE.md`),且 `ccteam init` **不**自动生成它 —— 给 Codex bot 设定行为要手建:

```bash
$EDITOR ~/projects/demo-api/AGENTS.md      # Codex 自动读取项目根 AGENTS.md
```

> 注意:`.claude/agents/<role>.md`(per-role 人格文件)目前**不**被聊天会话加载(聊天 adapter 起的是 `claude --name`,不带 `--agent`)。所以 per-bot 人格当前没有确定性落地路径 —— 要按场景区分行为,就**一个项目一个 bot + 一份项目级 `CLAUDE.md` / `AGENTS.md`**。

## 8. 日常操作(全命令速查)

shell 侧(`ccteam` CLI):

| 命令 | 作用 |
|---|---|
| `ccteam status` | daemon 是否活着 |
| `ccteam doctor` / `--verify-mcp` | 安装/依赖体检、MCP 工具自检 |
| `ccteam start` / `ccteam stop` | 起/停 gateway daemon |
| `ccteam init --slug <s>` | 初始化/纳管一个项目 |

Telegram 侧(网关命令,均为确定性):

| 命令 | 作用 |
|---|---|
| `/new claude reviewer` | 当前项目建 Claude session,handle `@reviewer` |
| `/new codex api` | 当前项目建 Codex session,handle `@api` |
| `/use s1` | 切到 session `s1` |
| `/cd demo-api` | 当前 chat 切到 `demo-api`;活动 session 跟着切(无则下条消息新建)|
| `/sessions` · `/projects` | 列当前 chat 的 session / daemon 已知项目 |
| `@api /compact` · `@api /review` | Codex 原生 RPC(compact / review) |
| `@reviewer /clear` | Claude TUI slash 透传 |
| `@ccteam status` · `cost` · `stop` | 网关管理(自然语言 admin) |

一个 chat 就是一台终端:里面同时操作多个项目、多个 session;不同 chat 互相隔离。

## 9. 手动凭证门:真实 Telegram roundtrip

真实 Telegram 验收需要你的 token 和 chat id,默认不跑。需要时执行:

```bash
CCTEAM_REAL_IM_TELEGRAM=1 \
CCTEAM_TELEGRAM_BOT_TOKEN='123456:replace_me' \
CCTEAM_TELEGRAM_CHAT_ID='123456789' \
bash scripts/smoke-im.sh --real
```

脚本会向该 chat 发唯一验证码,等你把同一验证码回给 bot,再发 PASS ACK。这是手动凭证门,不阻塞普通 ship gate。

## 10. 卡住时

先跑:

```bash
ccteam doctor
ccteam status
tail -80 /tmp/ccteam.log
```

再看 [troubleshooting.md](troubleshooting.md)。常见问题:Telegram chat id 不在 `allowed_chat_ids`、Claude/Codex CLI 未登录、daemon 没重启读取新 credentials。
