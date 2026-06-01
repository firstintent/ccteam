# Quickstart

目标:5-10 分钟从空机器跑到 Telegram 里第一条往返。你只需要一个本地仓库、Claude Code、Codex CLI 和一个 Telegram bot token。

## 1. 安装 CLI 和插件

先装 `ccteam` CLI:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version
```

如果脚本提示 `~/.local/bin` 不在 `PATH`,把它打印的 `export PATH=...` 加到 shell 配置后重新打开终端。

在 Claude Code 里安装 Claude 插件:

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

最后跑一次体检:

```bash
ccteam doctor
ccteam doctor --verify-mcp
```

`--verify-mcp` 应显示 `27 active, 0 stubs`。

## 2. 登录 Claude 和 Codex

`ccteam` 不打包 Claude 或 Codex 二进制,它调用你机器上的真实 CLI。

```bash
claude --version
codex --version
```

如果任一命令要求登录,按对应 CLI 的提示完成登录。之后再跑:

```bash
claude --version
codex --version
ccteam doctor
```

## 3. 初始化两个项目

一个 chat 可以同时切多个项目。下面用两个本地目录演示:

```bash
mkdir -p ~/projects/demo-app ~/projects/demo-api

cd ~/projects/demo-app
ccteam init --slug demo-app

cd ~/projects/demo-api
ccteam init --slug demo-api
```

`ccteam init` 会在项目里写入 `.ccteam/{agents,skills,state.json}` 和 `.claude/agents`。重跑是安全的;如果你明确要覆盖 ccteam 生成物,加 `--force`。

## 4. 配 Telegram bot

在 Telegram 里找 `@BotFather`,创建 bot 并复制 token。然后先给你的 bot 发一条消息,或把 bot 加进目标群后发一条消息,用下面命令拿 chat id:

```bash
BOT_TOKEN='123456:replace_me'
curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getUpdates"
```

在输出里找到目标 `chat.id`,写入本机凭证文件:

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

`allowed_chat_ids` 是安全边界:只允许这些 chat 触达 daemon。生产环境不要留空。

## 5. 启动 gateway daemon

```bash
ccteam start > /tmp/ccteam.log 2>&1 &
```

这个进程同时提供:

- IM gateway:Telegram 入站/出站长连接
- MCP socket:`~/.ccteam/run/mcp.sock`
- Web UI:`http://127.0.0.1:7331`

看状态:

```bash
ccteam status
tail -40 /tmp/ccteam.log
```

停机:

```bash
ccteam stop
```

`ccteam stop` 只停 daemon,不会杀 Claude tmux session。下次 `ccteam start` 会按 session id 接回。

## 6. 在 Telegram 里跑第一条消息

给 bot 发送:

```text
/pair phone
```

预期回复:

```text
paired phone
```

然后创建两个 session:

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

`@handle` 决定路由;不带 `@handle` 时消息发给当前 session。`/cd` 只切当前 chat 的当前项目,不会影响其他 Telegram chat。

## 7. 日常操作

常用 Telegram 命令:

| 命令 | 作用 |
|---|---|
| `/new claude reviewer` | 在当前项目创建 Claude session,handle 是 `@reviewer` |
| `/new codex api` | 在当前项目创建 Codex session,handle 是 `@api` |
| `/use s1` | 切到 session `s1` |
| `/cd demo-api` | 当前 chat 切到 `demo-api` 项目 |
| `/sessions` | 列当前 chat 的 session |
| `/projects` | 列 daemon 已知项目 |
| `@api /compact` | Codex 原生 compact RPC |
| `@api /review` | Codex 原生 review RPC |
| `@reviewer /clear` | Claude TUI slash 透传 |

一个 chat 就是一台终端:你可以在里面同时操作多个项目和多个 session。不同 chat 互相隔离。

## 8. 手动凭证门:真实 Telegram roundtrip

真实 Telegram 验收需要你的 token 和 chat id,因此默认不跑。需要验收时执行:

```bash
CCTEAM_REAL_IM_TELEGRAM=1 \
CCTEAM_TELEGRAM_BOT_TOKEN='123456:replace_me' \
CCTEAM_TELEGRAM_CHAT_ID='123456789' \
bash scripts/smoke-im.sh --real
```

脚本会向该 chat 发送唯一验证码,等待你把同一验证码回给 bot,然后发送 PASS ACK。这个门是手动/凭证门验收,不阻塞普通 ship gate。

## 9. 卡住时

先跑:

```bash
ccteam doctor
ccteam status
tail -80 /tmp/ccteam.log
```

再看 [troubleshooting.md](troubleshooting.md)。常见问题是 Telegram chat id 不在 `allowed_chat_ids`、Claude/Codex CLI 未登录、daemon 没重启读取新 credentials。
