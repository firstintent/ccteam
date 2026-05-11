# V0.3.1 用户使用与手动验证手册

本文用于手动验收 V0.3.1 的新增功能:HarnessAdapter / Codex stub /
`kind: flex` / adhoc multi-session / web flex UI。命令以仓库根目录为起点。

V0.3.1 的 Codex 只到 trait stub:能声明 `harness: codex`,CLI 也接受
`--harness=codex`,但运行时必须返回 V0.3.2 deferred error。看到这个失败就是
预期结果。

---

## 0. 准备

### 0.0 这份手册的验证边界

手动验证分两条线:

- **默认路径不需要跑 `ccteam start`**:flex team 创建、project 创建、
  `ccteam session add/ls/attach/rm`、statusline / harness snapshot、web 页面、
  SSE、screenshot 和写动作都可以直接验证。
- **只有验证 orchestrator 行为时才跑 `ccteam start`**:见第 6 节。`start` 会扫描
  projects root 下的项目,workflow 项目可能会启动真实 Claude session;不想触发旧项目时,
  请使用第 0.2 节的 `/tmp` 隔离环境。

### 0.1 选择二进制

如果已安装 `ccteam`,直接用:

```bash
export CCTEAM_BIN=ccteam
```

如果只想验证当前仓库 build:

```bash
cargo build --workspace
export CCTEAM_BIN="$PWD/target/debug/ccteam"
"$CCTEAM_BIN" --version
```

期望:

```text
ccteam 0.3.1
```

### 0.2 安全提示

以下命令会创建真实项目目录、tmux session,并在 `session add --harness=claude`
时启动真实 `claude --dangerously-skip-permissions`。它不会主动向 LLM 发 prompt,
但你 attach 后输入内容会进入真实 Claude Code session。

如果你想把 ccteam 数据隔离到 `/tmp`,可以设置:

```bash
export VERIFY_ROOT=/tmp/ccteam-v031-manual
export CCTEAM_HOME="$VERIFY_ROOT/ccteam-home"
export CCTEAM_PROJECTS_ROOT="$VERIFY_ROOT/projects"
export XDG_CONFIG_HOME="$VERIFY_ROOT/xdg-config"
mkdir -p "$CCTEAM_HOME" "$CCTEAM_PROJECTS_ROOT" "$XDG_CONFIG_HOME"
```

不要在真实 Claude Code 登录验证时设置 `CLAUDE_CONFIG_HOME`;否则新开的 Claude
Code session 会读隔离的配置目录,可能没有登录态。只验证 statusline install 不想
碰真实 `~/.claude` 时,才额外设置:

```bash
export CLAUDE_CONFIG_HOME="$VERIFY_ROOT/claude"
mkdir -p "$CLAUDE_CONFIG_HOME"
```

### 0.3 本手册用到的变量

```bash
export TEAM=manual-flex
export SLUG=manual-flex-check
export SID1=claude-1
export SID2=claude-2
export TEAM_DESCRIPTION="Manual V0.3.1 flex verification team"
export CCTEAM_HOME="${CCTEAM_HOME:-$HOME/.ccteam}"
export CCTEAM_PROJECTS_ROOT="${CCTEAM_PROJECTS_ROOT:-$HOME/projects}"
```

显式 `--slug manual-flex-check` 已经带 `<team>-` 前缀,所以实际 slug 仍是
`manual-flex-check`。如果传 `--slug check`,ccteam 会自动改成
`manual-flex-check`。

---

## 1. Flex team factory

### 1.1 创建 flex team

```bash
"$CCTEAM_BIN" team init "$TEAM" \
  --kind flex \
  --description "$TEAM_DESCRIPTION" \
  --author-name "${USER:-manual}"
```

期望:

- 输出 `ccteam team init`
- `phases (0)` 或无 phase 文件
- staging 目录在 `${XDG_CONFIG_HOME:-$HOME/.config}/ccteam/teams/$TEAM/`

检查 `team.yaml`:

```bash
TEAM_YAML="${XDG_CONFIG_HOME:-$HOME/.config}/ccteam/teams/$TEAM/team.yaml"
sed -n '1,80p' "$TEAM_YAML"
```

期望包含:

```yaml
name: manual-flex
kind: flex
description: Manual V0.3.1 flex verification team
phase_dir: phases
sessions:
  - sid: claude-1
    harness: claude
```

`--description` 不能省略。`team init` 会同时生成 Claude Code plugin manifest,
manifest 的 `description` 是非空必填字段;如果省略,会看到:

```text
Error: plugin manifest

Caused by:
    plugin.json: `description` must be non-empty
```

这是参数缺失,不是 flex team 本身失败。补上 `--description` 后重跑即可。

注意:`team.yaml::sessions[]` 是默认 session schema / 文档入口;当前 V0.3.1
运行时仍通过 `ccteam session add` 显式启动实际 tmux session。

### 1.2 校验与发布

```bash
"$CCTEAM_BIN" doctor --validate-team "$TEAM"
"$CCTEAM_BIN" team publish "$TEAM" --target local
```

期望:

- `doctor --validate-team` 对 flex team 跳过 phase IO / markdown checks
- `doctor` 末尾如果出现
  `[ccteam] codex CLI: not found (V0.3.1 trait-stub only; ...)`,这是
  V0.3.1 的 Codex 前向兼容探测,不计入 fail,也不影响 flex 验证
- `team publish --target local` 在 Claude Code local marketplace 下创建
  `ccteam-local/plugins/$TEAM` symlink
- `team publish` 输出里的 `linked → .../plugins/$TEAM` 就是成功信号;后面的
  `share with: ... claude /plugin add <staging-path>` 是给分发给其他机器时看的
  提示,本机 local marketplace 验证不需要再执行

如果设置了 `CLAUDE_CONFIG_HOME`,publish 目标在
`$CLAUDE_CONFIG_HOME/plugins/marketplaces/ccteam-local/plugins/$TEAM`。

---

## 2. 创建 flex project

```bash
"$CCTEAM_BIN" new "Manual V0.3.1 verification" \
  --team "$TEAM" \
  --slug "$SLUG" \
  --no-auto-slug
```

期望输出:

```text
created project manual-flex-check (team: manual-flex)
```

检查 state:

```bash
STATE="$CCTEAM_PROJECTS_ROOT/$SLUG/.ccteam/state.json"
sed -n '1,120p' "$STATE"
```

期望:

- `team` 是 `manual-flex`
- `team_kind` 是 `flex`
- `current_phase` 为空字符串
- `sessions` 还没有实际运行记录,直到下一节 `session add`

负向验证:

```bash
"$CCTEAM_BIN" session ls "$SLUG"
```

期望输出表头,但无 session 行。

---

## 3. Adhoc multi-session CLI

### 3.1 添加 Claude session

```bash
"$CCTEAM_BIN" session add "$SLUG" --harness=claude
```

期望:

```text
added session claude-1 (claude) for manual-flex-check
  tmux: ccteam-manual-flex-check-claude-1
  cwd: .../manual-flex-check/.ccteam/sessions/claude-1
```

检查:

```bash
tmux has-session -t "ccteam-$SLUG-$SID1"
"$CCTEAM_BIN" session ls "$SLUG"
sed -n '1,180p' "$STATE"
```

期望:

- tmux session 存在
- `session ls` 表格有 `claude-1`
- `state.json::sessions.claude-1.tmux_session` 是 `ccteam-$SLUG-claude-1`
- `state.json::next_sid_seq.claude` 为 `2`
- 项目下有 `.ccteam/sessions/claude-1/{inbox,outbox}/`

### 3.2 添加第二个 session

```bash
"$CCTEAM_BIN" session add "$SLUG" --harness=claude
"$CCTEAM_BIN" session ls "$SLUG"
```

期望新增 `claude-2`,tmux 名为 `ccteam-$SLUG-claude-2`。

### 3.3 Attach

```bash
"$CCTEAM_BIN" session attach "$SLUG" "$SID2"
```

期望进入对应 tmux session。detach 用 tmux 默认键:`Ctrl-b` 后按 `d`。

### 3.4 删除 session 与 sid 不复用

`session rm` 是 V0.3.1 唯一显式用户授权的 shutdown 路径;它会先走 harness
adapter 的 graceful shutdown,然后从 `state.json::sessions` 删除记录。

```bash
"$CCTEAM_BIN" session rm "$SLUG" "$SID1"
"$CCTEAM_BIN" session ls "$SLUG"
"$CCTEAM_BIN" session add "$SLUG" --harness=claude
"$CCTEAM_BIN" session ls "$SLUG"
```

期望:

- `claude-1` 被移除
- 新增 session 是 `claude-3`,不会复用 `claude-1`

### 3.5 Codex stub

```bash
"$CCTEAM_BIN" session add "$SLUG" --harness=codex
echo "exit=$?"
```

期望:

- exit code 是 `1`
- stderr 包含 `V0.3.2`
- stderr 包含 `docs/research/ccteam-codex-integration.md`

这是正确结果;V0.3.1 没有 Codex 真 spawn。

### 3.6 Workflow 项目拒绝 session 子命令

任选一个非 flex 项目 slug,或创建一个临时 dev 项目:

```bash
"$CCTEAM_BIN" new "Workflow rejection check" --team dev --slug dev-v031-check --no-auto-slug
"$CCTEAM_BIN" session ls dev-v031-check
```

期望失败,stderr 提到 session subcommands only work on flex teams。

---

## 4. Progress / harness snapshot 文件

### 4.1 per-session progress.jsonl

手动写一条 progress event:

```bash
mkdir -p "$CCTEAM_HOME/progress/$SLUG"
printf '%s\n' \
  '{"ts":"2026-05-11T00:00:00Z","event":"PostToolUse","tool":"ManualCheck"}' \
  >> "$CCTEAM_HOME/progress/$SLUG/$SID2.jsonl"

"$CCTEAM_BIN" session ls "$SLUG"
"$CCTEAM_BIN" progress "$SLUG" | tail -20
```

期望:

- `session ls` 的 `last_event` 显示 `PostToolUse`
- flex progress 文件路径是 `$CCTEAM_HOME/progress/$SLUG/$SID2.jsonl`
- 非 flex 项目仍使用旧路径 `$CCTEAM_HOME/progress/<slug>.jsonl`

### 4.2 statusline adapter install

真实启用 Claude Code statusline 双写:

```bash
"$CCTEAM_BIN" doctor --install-statusline-adapter
```

期望输出:

- `installed: .../.claude/statusline-command.sh`
- 如果原来已有 statusline command,会生成 `.bak-<utc>` 备份

检查 wrapper:

```bash
STATUSLINE="${CLAUDE_CONFIG_HOME:-$HOME/.claude}/statusline-command.sh"
grep -n "ccteam-managed:statusline" "$STATUSLINE"
grep -n "hook harness-snapshot" "$STATUSLINE"
```

### 4.3 手动触发 harness snapshot

不依赖真实 Claude Code statusline,直接模拟 statusline stdin:

```bash
SESSION_DIR="$CCTEAM_PROJECTS_ROOT/$SLUG/.ccteam/sessions/$SID2"
mkdir -p "$SESSION_DIR"
cd "$SESSION_DIR"

printf '%s' '{
  "model": {"display_name": "Manual Model"},
  "context_window": {"used_percentage": 12},
  "cost": {"total_cost_usd": 0.34},
  "rate_limits": {"five_hour": {"used_percentage": 7}},
  "cwd": "'"$SESSION_DIR"'"
}' | "$CCTEAM_BIN" hook harness-snapshot

cat "$CCTEAM_HOME/harness/$SLUG-$SID2.json"
```

期望 JSON 包含:

- `"harness":"claude-code"`
- `"model_display_name":"Manual Model"`
- `"context_used_pct":12`
- `"cost_usd_total":0.34`
- `"rate_limit_pct":7`

---

## 5. Web dashboard / flex UI

### 5.1 启动 web

```bash
"$CCTEAM_BIN" web --bind 127.0.0.1:7331
```

loopback bind 默认无 token。浏览器打开:

```text
http://127.0.0.1:7331/
```

如果端口被占用,换一个端口:

```bash
"$CCTEAM_BIN" web --bind 127.0.0.1:7332
```

### 5.2 Dashboard

打开 `/`,期望:

- 表格有 `Kind` 列
- `$SLUG` 行显示 `flex`
- flex 项目的 phase 显示 `manual` 或空 phase 兼容展示

### 5.3 Project detail

打开:

```text
http://127.0.0.1:7331/project/manual-flex-check
```

期望:

- 页面显示 `Sessions (N)`
- 每张 session card 有 sid、harness badge、状态、cost
- card 链接指向 `/session/$SLUG/$SID`
- screenshot fallback 链接指向 `/screenshot/$SLUG-$SID.png`

### 5.4 Session detail

打开:

```text
http://127.0.0.1:7331/session/manual-flex-check/claude-2
```

期望:

- header 显示 slug / sid / harness / tmux session
- events 列表只显示该 sid 的 progress
- harness snapshot 卡显示上一节写入的 `Manual Model`
- 页面含 sid-scoped `/btw`,pause,resume 表单

### 5.5 SSE:progress sid 过滤

开一个终端订阅:

```bash
curl -N "http://127.0.0.1:7331/sse/project/$SLUG/$SID2"
```

另一个终端追加该 sid 的 progress:

```bash
printf '%s\n' \
  '{"ts":"2026-05-11T00:01:00Z","event":"PostToolUse","tool":"SseManual"}' \
  >> "$CCTEAM_HOME/progress/$SLUG/$SID2.jsonl"
```

期望 curl 收到:

```text
event: progress
data: {"slug":"manual-flex-check","sid":"claude-2",...}
```

再追加到别的 sid:

```bash
printf '%s\n' \
  '{"ts":"2026-05-11T00:02:00Z","event":"PostToolUse","tool":"OtherSid"}' \
  >> "$CCTEAM_HOME/progress/$SLUG/claude-3.jsonl"
```

期望订阅 `$SID2` 的 curl 不显示 `claude-3` 事件。

### 5.6 SSE:harness snapshot sid 过滤

开一个终端订阅:

```bash
curl -N "http://127.0.0.1:7331/sse/harness/$SLUG/$SID2"
```

另一个终端重新触发 harness snapshot:

```bash
cd "$CCTEAM_PROJECTS_ROOT/$SLUG/.ccteam/sessions/$SID2"
printf '%s' '{
  "model": {"display_name": "Manual SSE Model"},
  "context_window": {"used_percentage": 33},
  "cost": {"total_cost_usd": 0.56},
  "rate_limits": {"five_hour": {"used_percentage": 9}}
}' | "$CCTEAM_BIN" hook harness-snapshot
```

期望 curl 收到:

```text
event: harness_snapshot
data: {"slug":"manual-flex-check","sid":"claude-2","snapshot":{...}}
```

### 5.7 Screenshot endpoint

```bash
curl -sS \
  -D /tmp/ccteam-v031-shot.headers \
  -o /tmp/ccteam-v031-shot.out \
  "http://127.0.0.1:7331/screenshot/$SLUG-$SID2.png"
sed -n '1,20p' /tmp/ccteam-v031-shot.headers
file /tmp/ccteam-v031-shot.out
```

期望二选一:

- 如果 tmux session 正在运行:HTTP 200,文件是 PNG
- 如果 tmux / pane 不可用:HTTP 504,body 是 `screenshot unavailable...`

504 是 graceful degrade,不是失败。

### 5.8 Web 写动作

`/btw` 写入 session inbox:

```bash
curl -i -X POST \
  -d "text=manual web nudge" \
  "http://127.0.0.1:7331/api/$SLUG/$SID2/btw"

find "$CCTEAM_PROJECTS_ROOT/$SLUG/.ccteam/sessions/$SID2/inbox" -maxdepth 1 -type f -print
```

期望:

- HTTP 303,Location 是 `/session/$SLUG/$SID2`
- session inbox 目录新增一条 markdown message

pause / resume:

```bash
curl -i -X POST "http://127.0.0.1:7331/api/$SLUG/$SID2/pause"
curl -i -X POST "http://127.0.0.1:7331/api/$SLUG/$SID2/resume"
```

期望都是 HTTP 303,回到 `/session/$SLUG/$SID2`。

非 loopback bind(`0.0.0.0:7331`)默认需要 token;浏览器先用 stderr 打印的
`?token=ccteam:<token>` URL 建 cookie,或者 curl 带:

```bash
curl -H "Authorization: Bearer ccteam:<token>" ...
```

---

## 6. Orchestrator flex 行为

本节是可选项,不是前面 flex team / multi-session / web 验证的前置步骤。
`session add` 会直接启动对应 tmux session;web 直接读取 state / progress /
harness snapshot 文件。因此只验用户可见功能时,不用跑 `ccteam start`。

需要专门验 orchestrator 时再跑本节。flex team 没有 phase DAG,手动验证重点是
"不注入 phase prompt,但观测面保留":

如果你按 3.6 创建了 `dev-v031-check` 只做负向验证,又不想让 orchestrator
启动这个 workflow 项目的真实 Claude session,先删掉它:

```bash
rm -rf "$CCTEAM_PROJECTS_ROOT/dev-v031-check"
```

```bash
"$CCTEAM_BIN" start --tick-seconds 2
```

保持该进程运行几十秒,观察:

- 不应向 flex session 注入 `plan-eng` / `implement` 等 phase prompt
- 不应创建新的 auto-loop cycle
- progress / cost / silence 相关 watcher 仍能读文件并反映到 web
- workflow 项目仍按旧 phase DAG 运行

如果只验证 flex UI / session CLI,不需要启动 orchestrator。

---

## 7. 清理

谨慎执行。以下命令会关闭本手册创建的 flex sessions:

```bash
"$CCTEAM_BIN" session rm "$SLUG" "$SID2" || true
"$CCTEAM_BIN" session rm "$SLUG" claude-3 || true
tmux ls | grep "ccteam-$SLUG" || true
```

如果你使用了 `/tmp` 隔离环境:

```bash
[ -n "${VERIFY_ROOT:-}" ] && rm -rf "$VERIFY_ROOT"
```

如果你在真实环境发布了 `$TEAM`,按需手动检查后删除:

```bash
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/ccteam/teams/$TEAM"
rm -f "${CLAUDE_CONFIG_HOME:-$HOME/.claude}/plugins/marketplaces/ccteam-local/plugins/$TEAM"
```

如果你安装了 statusline adapter 且想恢复原状态,先检查备份:

```bash
ls -1 "${CLAUDE_CONFIG_HOME:-$HOME/.claude}"/statusline-command.sh.bak-* 2>/dev/null
```

确认要恢复哪个备份后,再覆盖 `statusline-command.sh`。

---

## 8. 验收清单

- [ ] `ccteam --version` 是 `0.3.1`
- [ ] `team init --kind flex --description ...` 生成 `kind: flex`,无 phases,含 `sessions[].harness`
- [ ] `doctor --validate-team <team>` 对 flex team 通过
- [ ] `team publish --target local` 创建 ccteam-local marketplace symlink
- [ ] `ccteam new --team <flex>` 创建 `team_kind: flex` 项目
- [ ] `session add --harness=claude` 创建 `ccteam-<slug>-claude-1`
- [ ] `session add` 第二次创建 `claude-2`
- [ ] `session rm claude-1` 后再次 add 得到 `claude-3`
- [ ] `session add --harness=codex` exit 1 且指向 V0.3.2 deferred
- [ ] per-session progress 写入 `$CCTEAM_HOME/progress/<slug>/<sid>.jsonl`
- [ ] `doctor --install-statusline-adapter` 写 statusline wrapper
- [ ] `hook harness-snapshot` 写 `$CCTEAM_HOME/harness/<slug>-<sid>.json`
- [ ] dashboard `/` 显示 `Kind` 列和 flex 项目
- [ ] `/project/<slug>` 显示 session cards
- [ ] `/session/<slug>/<sid>` 显示事件、harness snapshot、写动作
- [ ] `/sse/project/<slug>/<sid>` 只推该 sid 的 progress
- [ ] `/sse/harness/<slug>/<sid>` 只推该 sid 的 harness snapshot
- [ ] `/screenshot/<slug>-<sid>.png` 返回 PNG 或 graceful 504
- [ ] `POST /api/<slug>/<sid>/btw` 写入 session inbox
- [ ] `POST /api/<slug>/<sid>/{pause,resume}` 返回 303
- [ ] 只验 CLI / web / SSE 时未启动 `ccteam start`;只在第 6 节 orchestrator 可选项中启动
