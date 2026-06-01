# ccteam User Manual

这份手册面向日常用户和运维:不用看源码,可以 install -> run -> use -> operate -> troubleshoot。

## 1. 核心模型:一个 chat 就是一台终端

ccteam gateway 把 IM 消息路由到真实 Claude/Codex session。核心对象只有三个:

| 对象 | 含义 |
|---|---|
| chat | Telegram 私聊或群聊。每个 chat 有自己的当前项目、当前 session 和 session 列表。 |
| project | 本地一个已 `ccteam init` 的项目目录,用 slug 标识。 |
| session | 一个可继续上下文的 agent 会话,属于某个 chat 和某个 project。 |

常用命令:

| 命令 | 说明 |
|---|---|
| `/pair <code>` | 将当前 chat 建立为可用入口,并确保默认 session 存在。 |
| `/cd <project>` | 当前 chat 切到项目。 |
| `/new claude <handle>` | 在当前项目创建 Claude tmux session。 |
| `/new codex <handle>` | 在当前项目创建 Codex app-server session。 |
| `/use <session-id>` | 当前 chat 切到已有 session。 |
| `/sessions` | 列当前 chat 的 session。 |
| `/projects` | 列 daemon 已知项目。 |
| `@handle <text>` | 把这一条路由到指定 session,并把它设为当前 session。 |

典型对话:

```text
/pair phone
/cd demo-app
/new claude reviewer
/cd demo-api
/new codex api
/sessions
/projects
@reviewer 总结 demo-app 的风险
@api /review
@api /compact
```

同一个 chat 可以同时活多个项目、多个 session。另一个 Telegram chat 有独立状态,不会串到这个 chat。

## 2. 双 harness

ccteam 按 vendor 使用最合适的执行方式:

| Vendor | Harness | slash 行为 |
|---|---|---|
| Claude | tmux TUI session | `/compact`、`/clear` 等按字面发送给 Claude TUI。 |
| Codex | app-server JSON-RPC | `/compact` 走 `thread/compact/start`;`/review` 走 `review/start`。 |

两种 harness 可以在同一个 chat 并发存在:

```text
/cd demo-app
/new claude reviewer

/cd demo-api
/new codex api

@reviewer /clear
@api /compact
@api /review
```

输出会统一成 IM 回复。gateway 会先回 `submitted <session> turn <id>`,随后把 assistant/error 事件通过同一条 outbound ledger 发回 IM。

## 3. 安装和首次运行

安装:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
ccteam --version
```

Claude 插件:

```bash
claude
```

Claude 会话内:

```text
/plugin marketplace add https://github.com/firstintent/ccteam
/plugin install ccteam
```

Codex 插件:

```bash
codex plugin marketplace add firstintent/ccteam
```

登录检查:

```bash
claude --version
codex --version
ccteam doctor
ccteam doctor --verify-mcp
```

项目初始化:

```bash
cd /work/demo-app
ccteam init --slug demo-app

cd /work/demo-api
ccteam init --slug demo-api
```

Telegram credentials:

```bash
mkdir -p ~/.ccteam/im
chmod 700 ~/.ccteam ~/.ccteam/im
$EDITOR ~/.ccteam/im/credentials.json
chmod 600 ~/.ccteam/im/credentials.json
```

内容:

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

启动:

```bash
ccteam start > /tmp/ccteam.log 2>&1 &
```

停止:

```bash
ccteam stop
```

## 4. 运维

### daemon 生命周期

`ccteam start` 是一个常驻 gateway daemon,同进程提供:

| 组件 | 默认位置 |
|---|---|
| IM gateway | Telegram long polling + outbound send |
| MCP socket | `~/.ccteam/run/mcp.sock` |
| Web UI | `http://127.0.0.1:7331` |
| Hook sink | 接收 Claude/Codex 事件,归一成进度和 IM 回复 |

推荐运行方式:

```bash
ccteam start > /tmp/ccteam.log 2>&1 &
ccteam status
tail -80 /tmp/ccteam.log
```

重启:

```bash
ccteam stop
ccteam start > /tmp/ccteam.log 2>&1 &
```

daemon 退出时不会杀 tmux session。重启后 gateway 会按持久化 session id 续上:

- Claude:重新接回 tmux TUI session。
- Codex:通过 app-server resume 接回 thread。
- outbound:未发送或失败的 IM 回复保存在 `~/.ccteam/imd/outbound.jsonl`,启动后重放。

### 状态和日志

| 你要看 | 命令/文件 |
|---|---|
| daemon 是否活着 | `ccteam status` |
| 安装和依赖 | `ccteam doctor` |
| MCP 工具表面 | `ccteam doctor --verify-mcp` |
| 最近 daemon 日志 | `tail -120 /tmp/ccteam.log` |
| outbound ledger | `tail -80 ~/.ccteam/imd/outbound.jsonl` |
| gateway session state | `~/.ccteam/imd/gateway-state.json` |
| 项目状态 | `<project>/.ccteam/state.json` |
| 业务进度 | `~/.ccteam/progress/<slug>.jsonl` |

### cost cap

成本按 vendor 记账。常用入口:

```bash
ccteam doctor --check-cost-orphan
```

Codex 和 Claude 的用量来源不同,短期和厂商控制台可能有小偏差。生产上用 per-vendor cap 控制日成本,把异常高的 session 先停用或 compact 后再继续。

## 5. 故障处理

gateway 的原则是:失败必须在 IM 里可见,不能静默挂住。

| 故障 | 用户看到 | 恢复方式 |
|---|---|---|
| agent 启动失败 | `gateway error: ...` | 看 daemon log,确认 `claude`/`codex` 在 `PATH` 且已登录。 |
| Claude tmux pane 死 | `gateway error: ...` | `ccteam stop && ccteam start`,再发同一 `@handle` 消息。 |
| Codex app-server socket 断 | `gateway error: ...` | 确认 `codex app-server` 可用;必要时重启 daemon。 |
| turn 超时 | `gateway error: turn timed out ...` | 稍后重试;如果反复出现,compact 或新建 session。 |
| Telegram 不回 | 无回复或仅有提交 ACK | 检查 token、chat id allowlist、daemon log 和网络。 |
| daemon 被 kill | 短暂离线 | 重新 `ccteam start`;session 和 outbound ledger 会恢复。 |

常用恢复动作:

```text
/sessions
/use s1
@reviewer /clear
@api /compact
/new claude reviewer2
/new codex api2
```

如果一个 session 的上下文已经不可用,直接建新 session。旧 session 不影响同 chat 的其他 session。

## 6. 安全

### skip permissions

IM 路径里的 Claude session 使用 `--dangerously-skip-permissions`。含义:

- 没有手机批准门。
- agent 可以按本机 Claude Code 权限直接执行允许范围内的操作。
- 这是 YOLO 模式;只把 bot 暴露给可信 chat。

### Telegram allowlist

`~/.ccteam/im/credentials.json` 的 `allowed_chat_ids` 是第一层边界:

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

生产建议:

- `allowed_chat_ids` 不留空。
- bot token 不进 git。
- daemon 只跑在你控制的机器上。
- Web UI 只绑 `127.0.0.1`,除非你明确配置反代和鉴权。

## 7. 配置参考

| 项 | 默认 |
|---|---|
| ccteam home | `~/.ccteam` 或 `CCTEAM_HOME` |
| 项目根集合 | `~/projects` 或 `CCTEAM_PROJECTS_ROOT` |
| Telegram credentials | `~/.ccteam/im/credentials.json` |
| daemon pid | `~/.ccteam/ccteam.pid` |
| MCP socket | `~/.ccteam/run/mcp.sock` |
| Web token | `~/.ccteam/web-token` |

Gateway 相关 env:

| Env | 用途 |
|---|---|
| `CCTEAM_HOME` | 隔离 daemon state、ledger、config。 |
| `CCTEAM_PROJECTS_ROOT` | 默认项目根目录。 |
| `CCTEAM_CLAUDE_BIN` | 覆盖 Claude CLI 路径。 |
| `CCTEAM_CODEX_BIN` | 覆盖 Codex CLI 路径。 |
| `CCTEAM_CODEX_APP_SERVER_TRANSPORT=stdio` | 用 `codex app-server --listen stdio://`。 |
| `CCTEAM_CODEX_APP_SERVER_SOCKET` | 指向 Codex app-server UDS。 |
| `CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS` | submit 超时。 |
| `CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS` | turn 等待超时。 |

## 8. CLI 参考

| 命令 | 用途 |
|---|---|
| `ccteam init [--slug NAME] [--in PATH] [--force]` | 初始化或刷新项目。 |
| `ccteam start [--no-web] [--no-clipboard]` | 启动 gateway daemon。 |
| `ccteam stop` | 优雅停止 daemon,保留 session。 |
| `ccteam status` | 查看 daemon 和项目摘要。 |
| `ccteam doctor` | 体检。 |
| `ccteam doctor --verify-mcp` | MCP 表面验收。 |
| `ccteam doctor --check-cost-orphan` | 成本 ledger 对账。 |
| `ccteam web --bind 127.0.0.1:7331` | 单独启动 Web UI。 |

## 9. MCP 工具表

`ccteam doctor --verify-mcp` 验收 27 个 active tools。用户日常主要用这些 group:

| Group | 用途 |
|---|---|
| `chat_` | 注册/注销 bot、发消息、查历史、重置 chat session。 |
| `advise_` | Claude + Codex 第二意见。 |
| `admin_` | 改 persona、加工具、列管理状态。 |
| `workflow_` | 面向项目自动化的底层控制工具。 |
| `screenshot` | 只读终端截图。 |

MCP 是给 Claude/Codex plugin 和自动化使用的接口。日常 Telegram 使用优先走 `/new`、`/cd`、`/sessions`、`@handle`。

## 10. 真实 Telegram 验收

凭证门需要真实 bot token 和 chat id:

```bash
CCTEAM_REAL_IM_TELEGRAM=1 \
CCTEAM_TELEGRAM_BOT_TOKEN='123456:replace_me' \
CCTEAM_TELEGRAM_CHAT_ID='123456789' \
bash scripts/smoke-im.sh --real
```

脚本发验证码到 Telegram,你把验证码原样发回,脚本收到后发 PASS ACK。这是手动/凭证门验收;普通本地 smoke 不需要真实 Telegram。
