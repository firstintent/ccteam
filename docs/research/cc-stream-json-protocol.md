# Claude Code VS Code 扩展的 stream-json 驱动协议 —— 对 ccteam 的启发

> 2026-06-10 实测研究。素材:本机 `anthropic.claude-code-2.1.170` VS Code 扩展(extension.js 2.2MB + webview/index.js 4.8MB,minified 逆向 grep)、对同一 native binary 的 stream-json 活体实验、`references/claude-code/`(claude-code-best 社区逆向重写源码,v2.4.0,语义与实测吻合,作旁证)。
> 结论一句话:**slash 不是 TUI 特性,是协议特性 —— prompt/local 两类 slash 在 stream-json 下原样可用(发纯文本即可);只有 dialog 类(local-jsx)不可用,而 VS Code 的解法是客户端原生 UI + control_request。ccteam 的"TUI slash 怎么办"这个 blocker 不存在。**

---

## 1. VS Code 扩展怎么跑 claude

实际进程(本机 ps 实拍):

```
claude --output-format stream-json --verbose --input-format stream-json \
  --max-thinking-tokens 31999 --permission-prompt-tool stdio \
  --setting-sources=user,project,local --permission-mode default \
  --allow-dangerously-skip-permissions --debug --debug-to-stderr \
  --enable-auth-status --no-chrome --replay-user-messages
```

要点:

- **没有 `-p`**:不是一问一答 one-shot,是**长驻进程**,stdin 永不关,一个 stdio 管道上跑整个多轮会话(等价 ccteam 的 tmux 长 session,只是介质从 PTY 换成 NDJSON 管道)。
- **stdout 纯 NDJSON**,debug 全走 stderr(`--debug-to-stderr`)—— 和 ccteam `mcp-serve` 的 wire 纪律一回事。
- `--permission-prompt-tool stdio`:权限审批反向 RPC 给客户端(见 §4)。
- `--replay-user-messages`:CLI 把它**接受到的 user 消息回显**到 stdout(客户端据此渲染权威 transcript,而非自己记一份)。
- `--setting-sources=user,project,local`:CLAUDE.md / skills / plugins 照常加载 —— vendor 原生知识层不动(与 ccteam 红线一致)。
- 每个 IDE 会话 = 一个这样的进程;resume 同样走 `--resume <id>`。

## 2. Wire 协议(NDJSON 双向)

客户端 → CLI(每行一个 JSON):

```json
{"type":"user","session_id":"","parent_tool_use_id":null,
 "message":{"role":"user","content":[{"type":"text","text":"..."}]}}
{"type":"control_request","request_id":"...","request":{"subtype":"interrupt"}}
```

CLI → 客户端:`system:init`(能力广播)、`assistant`/`user`(回显)消息、`stream_event` 增量、每轮一个 `result`、以及**反向** `control_request`(权限审批等)/`control_response`。

**`initialize` 握手**(extension 启动后第一个 control request),request 可带:`hooks`(hook 以**回调**注册,事件经 `hook_callback` control request 回给客户端!)、`sdkMcpServers`(客户端**进程内 host MCP server**,CLI 经 `mcp_message` 调用)、`agents`(per-session 注入 agent 定义)、`systemPrompt`/`appendSystemPrompt`。response 返回:`commands`(全部 slash:`{name, description, argumentHint}`)、`models`(含 effort 支持)、`agents`、`output_styles`、`account`、`pid`。

**control_request subtype 全集**(binary strings 实测):客户端→CLI:`initialize interrupt set_permission_mode set_model set_max_thinking_tokens mcp_message mcp_status mcp_set_servers reload_skills reload_plugins background_tasks stop_task rewind_files seed_read_state read_file get_usage get_context_usage get_settings apply_flag_settings generate_session_title remote_control channel_enable`;CLI→客户端:`can_use_tool`(权限)、`hook_callback`、`mcp_message`。

## 3. 核心问题:slash 在没有 TUI 时怎么实现

CLI 内部把 slash command 分三类(claude-code-best 源码 `src/commands/*`,与实测一致):

| 类型 | 数量级 | 例子 | stream-json 下 |
|---|---|---|---|
| `prompt` | 9 内置 + 全部 custom commands/skills | /init /review /security-review、`.claude/commands/*` | ✅ 展开成 prompt 进 turn,**纯文本发过去即可** |
| `local` | ~40 | /cost /clear /compact /context /usage | ✅ CLI 本地执行,结果以 assistant+result 回流 |
| `local-jsx` | ~80 | /model /mcp /login /hooks /permissions /help | ❌ 要弹 Ink 对话框,headless **直接不暴露**(init 列表里没有) |

**活体实验证据**(对 2.1.170 binary,`-p --input-format stream-json --output-format stream-json`):

1. `system:init` 的 `slash_commands` 列表 = skills + `clear compact context init review security-review usage insights …`;**没有** model/mcp/login/help —— dialog 类被过滤。
2. 把 `/cost` 当 user 文本发进去 → CLI 本地执行,返回 assistant 消息(订阅用量文本)+ `result`,`total_cost_usd: 0`,**没打 API** —— slash 解析执行完全在 CLI 端,客户端零实现。

**VS Code 客户端的分工**:

- webview 输入框的 slash 自动补全菜单 = `initialize` response 的 `commands`(fuzzy search 客户端做),选中只是把 `/name ` 插进输入框;提交时**几乎原样透传**(唯一客户端拦截:`/remote-control`、`/rc`;UI 上的 Compact 按钮也只是提交字面量 `"/compact"`)。
- dialog 类命令客户端**原生重实现**:模型选择器 UI → `set_model`,权限模式切换 → `set_permission_mode`,MCP 管理面板 → `mcp_*` 系列,登录 → `claude_authenticate`/OAuth 流。
- claude-code-best 给自家 mobile bridge 写的 `isBridgeSafeCommand`(src/commands.ts)把这个taxonomy说透了:**prompt 类天然安全;local 类显式 allowlist;local-jsx 一律挡** —— 注释里就是事故复盘:"iOS 发 `/model` 把宿主机的 Ink picker 弹出来了"。

**"当文本"的确定性合同**(processSlashCommand.tsx 源码,零模型参与):首词命中命令表 → 执行;未知但像命令名(`looksLikeCommand`:只含 `[a-zA-Z0-9:_-]`)且 `stat("/<name>")` 不存在 → 本地返回 `Unknown skill: <name>`、`shouldQuery:false`(不打 API);像路径或含其他字符 → 当纯文本进模型。专用 slash RPC 不存在(control subtype 已穷举)—— 文本就是官方机制,确定性由这套代码分支兜底。

**同一 codebase 里有两种远端 slash 政策**,按客户端信任度分层:
- **VS Code / SDK stream-json**(本地受信客户端,有 picker 约束输入)→ slash 全量透传当 user 文本;
- **mobile Remote Control bridge**(IM 形态远端)→ 入站默认 `skipSlashCommands: true` 全当纯文本,再经 `isBridgeSafeCommand` 白名单放行(prompt 安全 / local opt-in / local-jsx 人话拒绝 "isn't available over Remote Control";未知 `/shrug` 当文本,注释原话 "A mobile user typing /shrug shouldn't see Unknown skill")。

**ccteam IM 入站该抄 bridge 姿势而非 VS Code**:daemon 拿 initialize 命令表前置校验 + 白名单 —— known-safe 透传、known-unsafe 人话报错、unknown 当文本。对照今天:send-keys 本质同样是"当文本"(打进 TUI 输入框走同一解析器),还额外背 PTY 时序 / bracketed-paste / 对话框开着时文本打进对话框这几层真不确定性。

## 4. 权限 HITL(对 ccteam 最有含金量的部分)

`--permission-prompt-tool stdio` + `--permission-mode default` 下,**每次工具审批是 CLI→客户端的同步 RPC**:

```
control_request {subtype:"can_use_tool", tool_name, input, tool_use_id,
                 permission_suggestions?: [PermissionUpdate…],  // "always allow" 选项
                 blocked_path?, decision_reason?, agent_id?, …}
→ 客户端弹原生 UI → control_response {allow/deny + 可选持久化建议}
```

比 ccteam 现在的 `PermissionRequest` hook 路线语义更强:双向应答、带 always-allow 建议、带 tool_use_id/agent_id 定位、运行中可 `set_permission_mode` 热切换。`--allow-dangerously-skip-permissions` 只是允许客户端日后切到 skip 模式,默认仍走审批。

## 5. 对 ccteam 的启发

1. **"stream-json 下 slash 怎么办"这个历史 blocker 不存在**。IM 用户发的 `/compact /clear /context /cost` + 全部 skills/custom commands,当 user 文本写 stdin 即可,CLI 端执行,行为与今天 send-keys 透传等价(且 `/compact /new /clear` 完全透传的红线照守)。
2. **dialog 类 slash 本来就不该透传 IM** —— 今天 tmux 模式下用户在 IM 发 `/model` 会在 pane 里弹 Ink picker,IM 端什么都看不见,这是现架构的真实坑。stream-json 模式反而修了它(CLI 不暴露 → 可以人话报错);ccteam 对应解法 = IM 命令面(/role /use /new)+ web Settings,**形态上 ccteam 已经长对了**,与 VS Code 的"客户端原生 UI + control_request"同构。
3. **HITL 升级路径**:can_use_tool RPC 比 PermissionRequest hook 更强(见 §4)。daemon 天然就是"客户端",转 IM [同意][拒绝] 零阻抗。
4. **initialize 握手送的能力**:hooks 免写 `.claude/settings.local.json`(回调注册);`sdkMcpServers` 让 daemon 进程内 host ccteam 的 15 个 MCP 工具(免 `.mcp.json` 注册面);`agents` per-session 注入 role(vendor 官方接口,语义等同 `--agent` 自读,不触 No-prompt-injection 红线;但 `systemPrompt` 字段**有 ≠ 用**,红线照守)。
5. **代价 = ccteam 的 moat 本身**:没有 pane → 丢逐字节保真 web 终端、screenshot 工具、用户 attach 旁观/接管、TUI spinner/todo 渲染。ccteam 的护城河是 shell 形态(cloud-terminal),terminal mirror 是核心体验 —— **所以不是换协议,是加第二个 adapter**。
6. **落点(正确的做法)**:`HarnessAdapter` 第二实现 `ClaudeStreamJson`,与 `ClaudeTui` 并存共享 `CanonicalEvent`:IM-only 轻量 session / 未来 serverless 形态走 stream-json(更便宜:无 tmux、无 PTY、事件结构化、permission RPC 原生);要终端体验的 session 留 tmux。配合已有的 codex app-server 研究(`references/codex-desktop-app-analysis.md`),两家 vendor 的 GUI 协议同构(NDJSON/JSON-RPC 双向 + 服务端发起的 permission RPC + dialog 命令客户端化),`CanonicalEvent` 抽象闭环。
7. **`--replay-user-messages` 顺手解决 turns.jsonl 权威性**:gateway 的 append_turn 可以消费 CLI 回显(它接受了什么、slash 展开成了什么)而非记"我以为我发了什么"。
8. **出站 hook 链路在 stream-json 下整条不需要**:Stop→`result`(自带 cost/usage)、回复原文→`assistant` 消息、工具活动→流内 tool_use/tool_result 块、PermissionRequest→`can_use_tool` RPC;「hook→HTTP→daemon→文件」缩成「daemon 读 stdout→直写 progress/turns.jsonl」,sid pane-env 注入 + forwarder 管道全省。要 hook 语义也走 initialize 注册 wire 回调(hook_callback),不写 settings.local.json。**但生命周期耦合是结构性代价**:tmux session 不依赖 daemon 存活(daemon 重启,in-flight turn 照常跑完、hook 照常落盘);stream-json 子进程 stdout 一断 in-flight turn 即丢,恢复只到 `--resume <session-id>` 粒度 —— 这正是 tmux 路径保留 hook+file SoT 的根本理由。两 adapter = 两条采集链路汇同一 CanonicalEvent,hooks 降级为 ClaudeTui 专属。

## 6. 实测踩坑(运维红线)

headless 起 `claude -p`(user settings 全量加载)会把**全局 telegram 插件**也拉起来;与正在跑的会话共用同一 bot token 时疑似 getUpdates 长轮询互踢(Telegram 同 token 单消费者),导致**本研究 session 的 telegram MCP 当场掉线**。

**插件隔离旋钮(源码确认,vendor 原生,三档)**:

1. **精确**:`enabledPlugins` 是普通 settings 键,user → project → local 合并、后层覆盖(pluginLoader object-spread,"last one wins: local > project > user";`/plugin disable` 自己就走这条)。项目 `.claude/settings.local.json` 写 `{"enabledPlugins": {"telegram@claude-plugins-official": false}}` 即只关该插件、只在该项目。ccteam 已拥有 settings.local.json 写权 → 零新机制。
2. **粗**:`--setting-sources=project,local`,user 层整体不参与 → 全局插件全不加载(代价:全局 CLAUDE.md/偏好同丢;VS Code 全量传 `user,project,local` 正是为了保留)。
3. **全隔离**:`CLAUDE_CONFIG_HOME` 独立目录(ccteam 测试态已用)。

ccteam 落点:**不默认替用户关插件**(用户环境归用户,vendor-native 红线延伸);该做的是 doctor 检测「独占外部资源类插件 × 多 session spawn」组合时 warn + 给第 1 档一行 fix。研究型 headless 探针一律走 2/3 档。
