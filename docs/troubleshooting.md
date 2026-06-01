# ccteam 故障排查手册

先跑三条:

```bash
ccteam doctor
ccteam status
tail -120 /tmp/ccteam.log
```

`ccteam doctor --verify-mcp` 用来确认 MCP 表面是 `27 active, 0 stubs`。Telegram / gateway 问题优先看本手册;项目设计和内部接口看 `docs/interfaces.md`。

## 1. 安装和登录

### 1.1 `ccteam: command not found`

原因:CLI 不在 `PATH`。

修复:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
ccteam --version
```

### 1.2 `claude` 找不到或未登录

原因:Claude Code CLI 未安装、未登录,或当前 shell 没加载 PATH。

修复:

```bash
which claude
claude --version
claude
ccteam doctor
```

如果 `claude` 会话要求登录,先按 Claude Code 提示完成登录。

### 1.3 `codex` 找不到或未登录

原因:Codex CLI 未安装、未登录,或 app-server 不可用。

修复:

```bash
which codex
codex --version
codex
ccteam doctor
```

如果你使用 npm 版 Codex,真实 app-server 路径可用:

```bash
CCTEAM_CODEX_APP_SERVER_TRANSPORT=stdio ccteam start
```

### 1.4 插件安装后 Claude 看不到 MCP 工具

修复:

```bash
ccteam doctor --install-mcp
ccteam doctor --verify-mcp
```

然后在 Claude Code 里重载 MCP 或重启会话。

## 2. 初始化和 daemon

### 2.1 `ccteam init` 在项目里失败

先确认你在目标项目目录:

```bash
pwd
git status --short
ccteam init --slug my-project
```

在 ccteam 源码目录里自举需要显式确认:

```bash
ccteam init --force
```

### 2.2 `ccteam start` 立刻退出

看日志:

```bash
ccteam start > /tmp/ccteam.log 2>&1
tail -120 /tmp/ccteam.log
```

常见原因:

- `ccteam init` 没跑过。
- `~/.ccteam/im/credentials.json` JSON 格式错误。
- Web 端口 `7331` 被占用。
- Claude/Codex CLI 不可用。

### 2.3 `ccteam stop` 后还有 tmux session

这是预期行为。`ccteam stop` 只停 gateway daemon,不杀 Claude tmux session。下次 `ccteam start` 会接回。

### 2.4 机器重启后会话没续上

检查:

```bash
ccteam start > /tmp/ccteam.log 2>&1 &
tail -120 /tmp/ccteam.log
```

如果某个 session resume 失败,IM 会收到 `gateway error: ...`。重新发送同一 `@handle` 消息;仍失败时用 `/new claude <handle2>` 或 `/new codex <handle2>` 开新 session。

## 3. Telegram

### 3.1 BotFather token 拿不到

在 Telegram 找 `@BotFather`:

```text
/start
/newbot
```

如果超过 bot 数量上限,用 `/mybots` 删除不用的 bot。

### 3.2 不知道 chat id

先给 bot 发一条消息,或在群里发一条包含 bot 的消息。然后:

```bash
BOT_TOKEN='123456:replace_me'
curl -s "https://api.telegram.org/bot${BOT_TOKEN}/getUpdates"
```

找 `message.chat.id`。私聊通常是正数;群聊可能是负数。

### 3.3 Telegram 完全不回

检查顺序:

```bash
jq . ~/.ccteam/im/credentials.json
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
tail -120 /tmp/ccteam.log
```

确认:

- `bot_token` 是 @BotFather 最新 token。
- `allowed_chat_ids` 包含当前 chat id。
- bot 已收到过一条来自该 chat 的消息。
- 机器可以访问 `api.telegram.org`。

### 3.4 收到 `drop msg from non-allowed chat`

原因:chat id 不在 allowlist。

修复:

```bash
$EDITOR ~/.ccteam/im/credentials.json
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

### 3.5 真实 Telegram roundtrip 怎么验收

这是手动/凭证门验收:

```bash
CCTEAM_REAL_IM_TELEGRAM=1 \
CCTEAM_TELEGRAM_BOT_TOKEN='123456:replace_me' \
CCTEAM_TELEGRAM_CHAT_ID='123456789' \
bash scripts/smoke-im.sh --real
```

脚本会发唯一验证码。你需要把同一验证码回给 bot;脚本收到后发 PASS ACK。

## 4. Gateway 路由

### 4.1 `/new` 报 `unknown project`

当前 chat 的项目不存在或没有注册。先确认项目已初始化:

```bash
cd /work/demo-app
ccteam init --slug demo-app
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

然后在 IM:

```text
/projects
/cd demo-app
/new claude reviewer
```

### 4.2 消息发给了错误 session

用显式 handle:

```text
/sessions
@reviewer 这条给 Claude
@api 这条给 Codex
```

或切当前 session:

```text
/use s2
```

### 4.3 多个 bot 在一个 chat 里,不带 `@handle` 报歧义

这是预期保护。用:

```text
@reviewer hello
@api hello
```

### 4.4 `/cd` 影响了其他群吗

不会。当前 project/session 是按 chat 隔离的。另一个 Telegram chat 需要自己 `/pair`、`/cd`、`/new`。

## 5. Harness 故障

### 5.1 Claude tmux pane 死

用户会看到 `gateway error: ...`。恢复:

```bash
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

再在 IM 里发:

```text
@reviewer 继续
```

如果仍失败,新建:

```text
/new claude reviewer2
```

### 5.2 Codex app-server socket 断

用户会看到 `gateway error: ...`。确认 Codex 可用:

```bash
codex --version
CCTEAM_CODEX_APP_SERVER_TRANSPORT=stdio ccteam start
```

或重启 daemon 后重试:

```text
@api /compact
@api /review
```

### 5.3 turn 超时

用户会看到:

```text
gateway error: turn timed out ...
```

处理:

- 等 30 秒重试。
- 对长上下文 session 发 `/compact`。
- 反复超时时新建 session。
- 检查模型服务是否限流。

### 5.4 slash 命令没有效果

确认路由到正确 vendor:

```text
@api /review
@api /compact
@reviewer /clear
```

Codex 的 `/review`、`/compact` 走 app-server RPC。Claude 的 slash 发送给 TUI,需要 Claude session 正常活着。

## 6. Outbound 和日志

### 6.1 daemon 重启后重复收到一条消息

原因:outbound ledger 会重放 queued/failed 行,用于防止重启丢消息。少量重复优先保证可见性。

检查:

```bash
tail -80 ~/.ccteam/imd/outbound.jsonl
```

### 6.2 只收到 `submitted ...`,没有后续回复

检查:

```bash
tail -120 /tmp/ccteam.log
tail -80 ~/.ccteam/imd/outbound.jsonl
```

如果后续是超时,IM 会收到 `gateway error: turn timed out ...`。如果没有任何后续事件,检查 harness 是否还活着。

### 6.3 日志太少

前台跑一次便于观察:

```bash
RUST_LOG=info ccteam start
```

生产建议把 stdout/stderr 接到 systemd、supervisord 或 shell 重定向文件。

## 7. 安全

### 7.1 为什么没有批准弹窗

IM 路径是 `--dangerously-skip-permissions`:无批准门。只把 bot token 和 chat id 给可信用户;不要把 bot 加进不可信群。

### 7.2 想限制谁能用 bot

用 Telegram allowlist:

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

改完重启 daemon。

### 7.3 token 泄漏

立即在 @BotFather revoke token,更新 `~/.ccteam/im/credentials.json`,重启 daemon。确认 token 没进 git:

```bash
git status --short
git grep -n "123456:" || true
```

## 8. 验收命令

普通本地 smoke:

```bash
bash scripts/smoke-im.sh
```

真实二进制 + WS gateway:

```bash
CCTEAM_REAL_CODEX_RPC=1 \
CCTEAM_REAL_IM_WS=1 \
CCTEAM_REAL_IM_WS_NL=1 \
CCTEAM_REAL_IM_WS_RESTART=1 \
CCTEAM_REAL_IM_WS_FAULTS=1 \
bash scripts/smoke-im.sh --real
```

真实 Telegram 凭证门见 §3.5。
