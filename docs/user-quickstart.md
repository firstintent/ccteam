# ccteam 快速上手

> 这是 M0 单项目自治流水线的可操作手册。**当前 happy path 已端到端跑通**(实测两次连续 phase advance:`plan-eng → implement → test-author`,claude 自主产出代码 + 测试文件)。
>
> 多项目并发 / Telegram bot / 跨项目记忆 / Score 评分等承诺见 `docs/user-guide.md`,M1+ 落地。

---

## 1. 先决条件

| 工具 | 用途 | 验证 |
|---|---|---|
| Rust ≥ 1.85 | 编译 ccteam | `rustc --version` |
| `tmux` ≥ 3.0 | 长 session 容器 | `tmux -V` |
| `claude` CLI | 执行体 | `claude --version` |
| `jq`(可选) | 解析 `--format json` | `jq --version` |

WSL2 / Linux / macOS 都验证过。

---

## 2. 安装

M0 没有发布渠道,从源码装:

```bash
cd ~/workplace/agents/ccteam
cargo build --release

# 让 ccteam 进 PATH —— 三选一(任一即可)
# A. 软链(推荐:升级时只重 build,链接不动)
ln -sf "$(pwd)/target/release/ccteam" ~/.local/bin/ccteam

# B. cargo install(冻结当前版本到 ~/.cargo/bin)
cargo install --path crates/ccteam-cli

# C. 临时(只在当前 shell 有效)
export PATH="$(pwd)/target/release:$PATH"

# 验证
which ccteam
ccteam --version       # ccteam 0.0.1
```

注:**hook 子进程**调用 ccteam 时**不依赖 PATH**——`ccteam new` 把 ccteam binary 的绝对路径渲染进 `.claude/settings.json`,所以即使你只用 C 选项,产出项目里的 hooks 仍能找到 binary。但你**自己**用 ccteam 命令的时候,需要 binary 能在你 shell 的 PATH 上。

---

## 3. 一次性初始化

```bash
ccteam init
```

实际输出:

```
✓ created /home/<you>/.ccteam
✓ unpacked 6 phase templates → /home/<you>/.ccteam/phases

health check:
  claude   : 2.1.128 (Claude Code)
  tmux     : tmux 3.2a
  ccteam   : /home/<you>/.local/bin/ccteam

next:
  ccteam new "<your one-line request>"
  ccteam start --foreground   # in another terminal
```

`init` 做的事:
- 建 `~/.ccteam/{phases,progress,inbox,control,queue,memory,state,log}`
- 解压 6 个 phase 模板到 `~/.ccteam/phases/`(`02-plan-eng.md` … `09-ship.md`)
- 体检 claude / tmux / ccteam binary 路径

幂等:再跑不会覆盖已经在的 phase 模板(用户手改不丢)。强制覆盖加 `--force`。

---

## 4. 一句话立项

```bash
ccteam new "Build a tiny CLI doubling a number"
```

输出:

```
created project tiny-cli-doubling-a-number
  spec   : /home/<you>/projects/tiny-cli-doubling-a-number/.ccteam/spec.md
  state  : .../state.json
  config : .../.claude/settings.json

run `ccteam start --foreground` (in another terminal) to dispatch the first phase.
```

**自动做的事**(都是 M0 修过的坑):

- slug 在 `-` 边界截断,不切到一半("…CSV to JSON" → `…converts`,不会变成 `…converts-cs`)
- `.ccteam/phases/` 拷 6 个 phase 模板(去掉 `02-` 编号前缀)——phase prompt 里 `@.ccteam/phases/<phase>.md` 现在能找到文件
- `.claude/settings.json` 渲染**绝对路径**到所有 hook 命令——子进程不依赖 PATH
- `.claude/settings.json` 注入 `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` 到 env 块——hook 子进程不依赖 tmux server 的 env 传播
- `~/.claude.json` 写 `projects[<dir>].hasTrustDialogAccepted = true`——首次启动跳过 "Trust this folder?" 阻塞选单

---

## 5. 启动 orchestrator

```bash
# 终端 1
ccteam start --foreground
```

发生什么:
1. 加载 phase 模板,做 M0 校验(parallelism 必须是 `solo`)
2. 启动 inotify 监听 + 30s tick(`--tick-seconds 4` 可调快)
3. 没项目就给提示
4. 有项目 + phase 是 `pending`(idle):
   - `ensure_session` 确保 tmux session 在,不在就 `tmux new-session ... claude --dangerously-skip-permissions`
   - 等 SessionStart hook 写 `.ccteam/ready` 标记
   - 3 秒 warmup(等 claude 的 TUI 准备好接收输入)
   - send-keys phase prompt(idle 状态直接发,忙时用 `/btw` 排队)
5. claude 干活;hooks 把 PreToolUse / PostToolUse / Stop 等事件写到 `~/.ccteam/progress/<slug>.jsonl`
6. claude 输出 `PHASE_DONE: <phase>` 后,Stop hook 跑 `parse-phase-end` 解析(优先用 stdin 的 `last_assistant_message`,兜底从 transcript 找——避免 transcript flush race)
7. orchestrator 看到 `phase_done` 事件 → 推进状态机 → 调度下一 phase

**关键修复**(M0 实测发现):
- 编排层不再死在「session 不存在」的 send_keys 错误——`ensure_session` 自动起来
- `parse-phase-end` 解析 Claude Code 2.x 的 transcript schema(`type: "assistant"` + nested `message`),不再因为 schema mismatch 永远找不到 PHASE_DONE
- `cost-accumulate` 也用相同 schema 修复——`ccteam show` 能看到真实 cost 累积
- `decide_tick` 扫描多个事件,不被「Stop → phase_done → SubagentStop」尾部的 SubagentStop 干扰

---

## 6. 旁观 / 干预

```bash
# 终端 2
ccteam ls
# SLUG                                  PHASE          STATE       COST   AGE
# tiny-cli-doubling-a-number            implement      in_flight   $3.64  120s

ccteam show tiny-cli-doubling-a-number
ccteam show tiny-cli-doubling-a-number --format json | jq

ccteam progress tiny-cli-doubling-a-number --tail   # 流式看 hook 事件

ccteam attach tiny-cli-doubling-a-number             # tmux attach 看 claude 实时;Ctrl+b d 退出
ccteam peek tiny-cli-doubling-a-number               # 不 attach 截屏 pane
```

如果 escalate 了(fix-loop 三轮没过 / cost > $200),修底层问题后:

```bash
ccteam resume tiny-cli-doubling-a-number             # 重置 phase_state=idle,下一 tick 会重新 dispatch
```

---

## 7. 命令参考

| 命令 | 作用 | 备注 |
|---|---|---|
| `ccteam init [--force]` | 建 ~/.ccteam/ + 解压 phase 模板 + 体检 | 幂等 |
| `ccteam new "<request>"` | 创建项目 | 自动 phase 模板拷贝 + trust + abs 路径 |
| `ccteam new --file path.md` | 从文件读需求 | |
| `ccteam ls [--format json]` | 列项目 | json 给 ccteam-control skill 用 |
| `ccteam show <slug> [--format json]` | 项目详情 | |
| `ccteam attach <slug>` | tmux attach | Ctrl+b d 退出不停 session |
| `ccteam peek <slug>` | 截屏 pane 不 attach | 不抢键盘 |
| `ccteam progress <slug> [--tail]` | dump progress.jsonl | tail 跟踪 |
| `ccteam resume <slug>` | 重置 phase_state=idle | escalate 后人工修完跑这个 |
| `ccteam start --foreground` | orchestrator daemon | M0 唯一支持的模式 |
| `ccteam start --tick-seconds 4` | 调试用快 tick | 默认 30 |
| `ccteam hook <subcmd>` | hook handler | 由 claude 调,人不直接用 |

环境变量:

- `CCTEAM_HOME`(默认 `~/.ccteam/`)
- `CCTEAM_PROJECTS_ROOT`(默认 `~/projects/`)
- `RUST_LOG=info,ccteam_core=debug`(orchestrator 调试)

---

## 8. 调试 cheat sheet

```bash
# hook 没在跑?
which ccteam                                         # 必须能找到
jq '.hooks.SessionStart[0].hooks[0].command' ~/projects/<slug>/.claude/settings.json
# 应该是绝对路径

# 看 hook 实际收到什么 stdin(临时调试):
# 把 .claude/settings.json 里某个 hook 命令前包一层 tee 即可

# 强制走完一轮快速验证(脱离 claude)
cargo test --release -p ccteam-core --test e2e_happy_path_test

# 真 claude e2e
RUST_LOG=info ccteam start --foreground --tick-seconds 4
# 另一终端
ccteam new "..."
# 再另一终端
ccteam progress <slug> --tail

# 看 phase advance
RUST_LOG=info ccteam start --foreground --tick-seconds 2 2>&1 | grep "phase advanced\|escalated"

# state.json 损坏?
ls ~/projects/<slug>/.ccteam/state.json{,.bak,.tmp}
jq . ~/projects/<slug>/.ccteam/state.json

# transcript 失踪?(claude 自己存的位置)
ls /home/<you>/.claude/projects/-<sluggified-cwd>/*.jsonl
```

---

## 9. 已知未实现 / 留作 M1+

不是 bug,是路线图:

- **`ccteam stop`**:M0 用 Ctrl-C 关 orchestrator,tmux session 留下;M1 加优雅停机
- **多项目并发**:M0 顺序处理,M1 上 `max_concurrent_projects` + 排队
- **Telegram bot 入口**:M1
- **`ccteam-control` skill / `ccteam-mcp` MCP server**:M1 / M2(让用户在自带 claude 里用对话方式调度 ccteam)
- **Seed phase + REJECT/CLARIFY**:M2
- **跨项目记忆 + RAG**:M3
- **Critic agent + score**:M4
- **`parallelism: agent_team` / `multi_session`**:M2 / M3

也有几个**没 spec 的小尾巴**应该排进 backlog:

- `ccteam ls --format json` 的 `running` 字段当前是 `null`(M1 加 daemon-liveness 检测)
- cost rate 表写死在 `cost.rs`,M1 改读 `~/.ccteam/config.yml`
- stall 软告警每 tick 都打,M1 接 telegram 时去重

---

## 10. 卸载

```bash
# 软链
rm ~/.local/bin/ccteam
# 或 cargo install
cargo uninstall ccteam-cli

# 数据
rm -rf ~/.ccteam/                    # 全局(只 ccteam 元数据,不影响项目源码)
rm -rf ~/projects/<slug>/            # 单项目(谨慎,会删源代码)
# tmux session(只杀 ccteam-* 前缀的,不动你其他 session)
tmux ls 2>/dev/null | awk -F: '/^ccteam-/ {print $1}' | xargs -r -n1 tmux kill-session -t
```

---

**改坑历史**:本文档前一版列了 10 条 M0 阻塞性 bug——本版本下面这些已经修了,代码里有 regression 测试守门:

| # | 修复点 | 测试 |
|---|---|---|
| 1 | hook 命令用 ccteam binary 绝对路径 | `templates::tests::template_session_start_uses_absolute_ccteam_path` |
| 2 | orchestrator `ensure_session` 自动起 tmux | `orchestrator_test::ensure_session_starts_a_missing_session` |
| 3 | bootstrap 拷贝 phase 模板到项目 | `projects::tests::bootstrap_project_writes_phase_templates_into_dot_ccteam_phases` |
| 4 | `ccteam init` 命令 | `commands::tests::run_init_*` |
| 5 | 自动信任项目目录 | `projects::tests::write_trust_entry_*` |
| 6 | slugify `-` 边界回退 | `projects::tests::slugify_rolls_back_to_dash_boundary_when_cut_would_split_word` |
| 7 | `ls`/`show` 显示 `pending` | `display_phase` 单元 |
| 8 | tmux/ 空目录删除 | — |
| 9 | `ccteam start` 友好提示 | — |
| 10 | (bonus) Claude Code 2.x transcript schema | `parse_phase_end_handles_claude_code_2x_schema`, `cost_accumulate_handles_claude_code_2x_schema` |
| 11 | (bonus) prefer stdin `last_assistant_message`(transcript flush race) | `parse_phase_end_uses_stdin_last_assistant_message_when_present` |
| 12 | (bonus) settings.json 注入 CCTEAM_HOME(tmux server 老 env 不传) | `render_project_settings_injects_ccteam_env_when_provided` |
| 13 | (bonus) post-ready warmup(claude TUI 没准备好接 send-keys) | `OrchestratorConfig::post_ready_warmup` |
| 14 | (bonus) `decide_tick` 扫多个事件(SubagentStop 在 phase_done 之后) | `latest_terminal_event_finds_phase_done_when_subagent_stop_is_last` |
