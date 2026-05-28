# V0.8 rmux 分支用户操作手册

> 面向想跑 `v0-8-rmux-integration` 分支的终端用户。**评估分支**(no-release / no-PR),用法以下文为准。

## 1. 装

```bash
git clone https://github.com/firstintent/ccteam /tmp/ccteam-rmux
cd /tmp/ccteam-rmux && git checkout v0-8-rmux-integration
cargo install --path crates/ccteam-cli --locked        # 装 ccteam 到 ~/.cargo/bin
ccteam --version                                       # 应显示 0.6.8(branch 内部 ver)
```

## 2. 初始化项目

```bash
cd <your-project>
ccteam init                                            # 落 .ccteam/ + workflow.yaml + .mcp.json
ccteam doctor --install-hooks                          # 装 hook.sh + 注册 chat-progress hooks
ccteam doctor --verify-mcp                             # 验 MCP 27 工具齐(0 STUB)
```

## 3. backend 选择(rmux 默认)

```bash
# 默认 = rmux,什么都不用设。
# 显式 opt-out 到 tmux:
export CCTEAM_MUX_BACKEND=tmux

# 验当前 backend:
ccteam doctor 2>&1 | grep -i backend
```

## 4. 启用 typed-event 管线(可观测性)

```bash
# 两个 flag 都要打开:
export CCTEAM_TYPED_EVENTS=1                           # 开 producer + consumer
export CCTEAM_HOOK_VIA_DAEMON=1                        # 走 daemon UDS 路 hook(必需)

# 都不打 = baseline 行为,完全跟 V0.7 一致。
ccteam start                                           # 启 orchestrator + IMD + daemon
```

## 5. 看 `progress.jsonl`

```bash
tail -f .ccteam/<slug>/progress.jsonl | jq -c .

# V0.8 你会新看到的行(其它老行不变):
# Claude 侧:
#   {"kind":"typed_event","event_kind":"rate_limit","vendor":"claude",...}
#   {"kind":"typed_event","event_kind":"idle",...}
#   {"kind":"merger_lossy_partial","event_kind":"turn_done",...}    # hook 丢失时
#   {"event":"chat_tool_call_started","tool":"Edit",...}            # PreToolUse 触发
#   {"event":"chat_tool_use","tool":"Edit",...}                     # PostToolUse 触发
# Codex 侧(mode-3 app-server):
#   {"kind":"typed_event","event_kind":"tool_call_started","vendor":"codex","captured":"<item_id>",...}
#   {"kind":"typed_event","event_kind":"turn_done","vendor":"codex",...}
```

## 6. 调试单事件

```bash
# 只看 typed_event:
jq -c 'select(.kind=="typed_event")' .ccteam/<slug>/progress.jsonl

# 只看 hook 丢失兜底(诊断 hook 子进程崩溃):
jq -c 'select(.kind=="merger_lossy_partial")' .ccteam/<slug>/progress.jsonl

# Codex 侧 tool-call(按 item_id 对账):
jq -c 'select(.kind=="typed_event" and .vendor=="codex" and (.event_kind|startswith("tool_call_")))' \
    .ccteam/<slug>/progress.jsonl
```

## 7. 回滚到 V0.7 行为

```bash
unset CCTEAM_TYPED_EVENTS CCTEAM_HOOK_VIA_DAEMON       # 关 typed-event
export CCTEAM_MUX_BACKEND=tmux                         # 回 tmux backend
ccteam stop && ccteam start                            # 重启
```

## 8. 常见排错

```bash
# 1. typed_event 行没出来 → 两 flag 都打了吗?
env | grep -E 'CCTEAM_TYPED_EVENTS|CCTEAM_HOOK_VIA_DAEMON'

# 2. PreToolUse hook 没触发 → 现网项目重跑 installer:
ccteam doctor --install-hooks --force
cat .claude/settings.json | jq '.hooks.PreToolUse'    # 应有一条 "ccteam mux hook-emit ..."

# 3. WSL2 inotify 触顶(`daemon_dm_* / daemon_wires_*` 偶现 flake):
echo 'fs.inotify.max_user_instances=512' | sudo tee -a /etc/sysctl.conf && sudo sysctl -p

# 4. rmux daemon 残留 → 按 PID 清(别 pkill -f,会 self-match):
ps -ef | awk '$0 ~ /ccteam --__internal-daemon/ && $0 !~ /awk/ {print $2}' | xargs -r kill

# 5. 端到端验证(跑 11 个 ignored 集成测试,~3 分钟):
cd /tmp/ccteam-rmux && bash scripts/rmux-smoke.sh
```

## 9. 本分支已知限制

| 项 | 状态 |
|---|---|
| macOS / Windows | 本地没验证。Windows 走 WSL2,macOS 走 CI matrix |
| 同工具并发(两个 Edit 并行) | grace 窗口内仍可能 mis-pair(同工具内部 FIFO 兜底);跨工具(`Edit + Read`)绝不 cross-pair |
| Codex `codex exec --json` (mode-2) | typed-event 未接,只 mode-3 app-server 接管线 |
| Codex `turn/plan/updated` | 语义与 Claude PlanPending 不一致,不映射 |

## 10. 升级到下次

```bash
cd /tmp/ccteam-rmux && git pull origin v0-8-rmux-integration
cargo install --path crates/ccteam-cli --locked --force
ccteam doctor --install-hooks --force                 # 同步 hook.sh 最新版
```

---

详:
- 架构 / 设计:`docs/versions/v0-8-rmux/as-built-architecture.md`
- 各 slice 设计:`w-slice-{3,4}-*.md`
- 当前状态:`SESSION-HANDOFF.md`
