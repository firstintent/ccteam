# V0.4.1 运维痛点记录(本轮 session 实战累积)

> 这是下一轮 UX 优化要扫的清单。**所有条目都是我在 ssh 调试 deploy host
> 时实际撞到的不顺畅**,不是想象的需求。优先级标在每条前面。
> 优先级:P0 = 我每次操作都撞;P1 = 撞了几次;P2 = 撞了一次但确实是 UX 漏洞。

---

## P0 — 重启/清场流程太多步

**症状**:更新 binary 后想重启 orchestrator,我每次都得跑这一串:

```bash
./target/release/ccteam stop                      # 1
sleep 2                                           # 2
pgrep -af "ccteam start" | grep -v bash           # 3 verify
pkill -9 -f "ccteam start"                        # 4 if 3 没空
rm -f ~/.ccteam/state/orchestrator.pid            # 5 清 stale pidfile
nohup ./target/release/ccteam start \
   > /tmp/ccteam.log 2>&1 < /dev/null & disown    # 6 重启
sleep 3 && tail -5 /tmp/ccteam.log                # 7 verify
```

**已修复的部分**:`b197f93` 修了 SIGTERM 被 watcher 吸收导致不退出的根因。
但即使 SIGTERM 现在能干净退出,重启仍然是 7 步。

**需要的命令**:

```bash
ccteam restart                  # = stop + wait-for-exit + start
ccteam restart --force          # = pkill -9 if SIGTERM 超时 + 清 pidfile + start
ccteam stop --wait              # blocks 到进程真退出再返回
```

理想的 deploy 升级流程:

```bash
git pull && cargo build --release -p ccteam-cli && ccteam restart
```

一行。

---

## P0 — `--bg` 任务的状态发现要 ssh + jq

**症状**:agent "stuck on a startup dialog" 这种状态只在 `~/.claude/jobs/<id>/state.json`
里的 `detail` / `tempo` 字段里。ccteam 自己不知道。我每次诊断都得:

```bash
ssh rob@host
d=$(ls -t ~/.claude/jobs/ | head -1)
jq '{state,detail,tempo,intent,cwd,output}' ~/.claude/jobs/$d/state.json
```

更糟:要把当前活跃 job 关联回 ccteam 项目,要 `find` 所有 state.json + 按 `cwd` 匹配项目目录。

**需要的**:

1. `ccteam jobs` — 列所有 ccteam 关联的 `claude --bg` job,带 `state` / `detail` 摘要
2. orchestrator 每 tick 读 state.json::`detail`,变化时 append 到 progress.jsonl
   (这样 `ccteam progress <slug>` 就能看到 "stuck on dialog" / "starting…")
3. `ccteam show <slug>` 突出显示每个 agent 的 state.json `state` / `detail`

---

## P0 — 旧 marker / 旧 inbox 没清理命令

**症状**:有 stale `spawn_requests/*.json`(老 binary 持续重试失败)。手动:

```bash
rm -rf ~/projects/<slug>/.ccteam/spawn_requests/*.json
rm -rf ~/projects/<slug>/.ccteam/inbox/*.md
```

**需要的**:

```bash
ccteam clear <slug>                          # 全清(谨慎)
ccteam clear <slug> --spawn-requests         # 只清 spawn marker
ccteam clear <slug> --inbox                  # 只清 inbox
ccteam clear <slug> --archived               # 清归档(inbox.archived/)
```

---

## P1 — Heredoc 通过 ssh 反复出 shell-expansion bug

**症状**:这一轮我至少撞了 2 次。`<<'EOF'` 单引号 vs 无引号、`$( ... )` 在
sshcommand 内层 vs 外层 expansion。要写一个带 `$(date)` 的文件就得来回试。

**Lesson(给我自己)**:别用 heredoc 跨 ssh。要么 scp 文件,要么 `echo
'literal-string' > target`。

**需要的产品改动**:`ccteam send` / `ccteam spawn` 已经覆盖了大部分手写场景
(`871d485`)。但 `workflow.yaml` 还得用户手写。

---

## P1 — 测试在 deploy host 上每次重编

**症状**:我每次 push 都 ssh + `cargo build --release`,~60-90s。三次更新就是 4-5 分钟纯等。

**需要的**:

1. scp 预编 binary 上去,免编译(代价:架构必须匹配)
2. 或者 host 上挂 sccache
3. 长期:CI 给 main 出 release artifact,host 直接拉

---

## P1 — 多个 mcp-serve 没法一眼看清归属

**症状**:8 个 mcp-serve 进程,我得用 ps + ppid 推断哪些是孤儿。

**已修复**:`b197f93` 加了 `PR_SET_PDEATHSIG(SIGTERM)`,新 mcp-serve 会在 parent
死时自杀。但现存遗留的还得 manual pkill。

**需要的**:

```bash
ccteam processes              # 列所有 ccteam-spawned proc + 标识孤儿
ccteam processes --orphans    # 只列孤儿
ccteam processes --kill-orphans   # 杀孤儿
```

---

## P1 — Workflow.yaml 没 scaffold

**症状**:`ccteam new` 创建项目但 workflow.yaml 要用户从零写。新用户怎么知道
schema?目前要去 grep `examples/workflows/*.yaml` 或读 PRD §6。

**需要的**:`ccteam new` 默认写一个 `workflow.yaml.example` + 4 个空 `.claude/agents/<role>.md`,
带 inline 注释解释 schema。

---

## P2 — Daemon 健康不够详细

**症状**:`ccteam status` 现在能看到 "daemon down"。但当 daemon UP 时,看不到:
- 在跑的 project 数量
- 监听的 watch path 数量
- 上一 tick 时间

**需要的**:扩展 `ccteam status`,daemon UP 时多 print 几行:

```
daemon: healthy (heartbeat 3s ago)
  rostered: 4 projects (dev-ui-quality, foo, bar, baz)
  watching: 7 paths total across 4 projects
  last tick: 3s ago
```

---

## P2 — 文档跟代码漂

**症状**:`docs/versions/v0-4-0/user-manual.md` 早就跟现实不一致(我这次 patch 了 §9.5
inbox 行为)。`ccteam send` / `spawn` / `status` / init wizard 这一轮加的还没文档化。

**需要的**:

1. 写 `docs/versions/v0-4-1/user-manual.md` 反映 V0.4.1 整套 UX
2. 或者考虑 `ccteam --help` + 各子命令 help 作为 SoT,文档自动从 `clap` 生成

---

## P2 — Web token 复制不方便

**症状**:`b5ab321` 的 banner 在 stderr 第一屏打出 `URL: http://x.x.x.x:7331/?token=ccteam:abc`。
但 stderr 跟 orchestrator log 混在一起,banner 滚走后想再 copy 就得 `cat ~/.ccteam/web-token`。

**需要的**:
- `ccteam status` 显示 token(已做 `3d7ceb2`)。够用。
- 或者 banner 同时 echo 到一个固定文件(已有 `~/.ccteam/web-token`),
  banner 只是友好提醒。够用。

**可关闭**。

---

## P2 — `ccteam attach` 自动判断要等 progress 事件

**症状**:`b5ab321` 让 `ccteam attach` 在没 tmux session 时 fallback 到
`claude attach <bg-id>`。但 bg-id 解析是去找 `~/.claude/jobs/<id>/state.json::cwd`
匹配项目目录的最新 `state == working` 的。这个 walk 在 jobs 多了之后 (>100 个)
会慢。

**需要的**:把 bg-id 记到 ccteam 的 `state.json` 里(`spawn` 时就持久化),
attach 时直接读,不要扫 ~/.claude/jobs。

---

## P2 — 没有"近期 cost" 视图

**症状**:agent 一跑就烧 token。`progress.jsonl` 里有 `cost_usd` 字段,但要 sum 起来。

**需要的**:

```bash
ccteam cost                      # 全 project 累计
ccteam cost <slug>               # 单 project
ccteam cost --today              # 今天
```

或者集成进 `ccteam status`。

---

## P2 — orchestrator 启动跑 `claude --bg` 的旧 spawn_requests 没清

**症状**:我重启 orchestrator 后,旧 marker(老 binary 留下,带 `--workdir` 失败的)
立刻被新 binary 试一遍。新 binary 是好的所以没事,但如果 marker 本身已过时
(role 不存在 etc)就是噪音。

**需要的**:orchestrator 启动时打印 "consuming N stale spawn_requests / inbox messages"
作为提醒,让 user 决定要不要 `ccteam clear` 一次。

---

---

## V0.4.1 deploy-verify 追加(2026-05-14 host 192.168.1.19)

下面这一批是把 `892f5e1` 部署到 NAS host 真机跑 8 项 UX 验证时发现的。
8/8 简化都过,但暴露了几条 V0.4.2 候选的 UX 漏洞。详细 verify 表见
`docs/versions/v0-4-1/deploy-verify.md`。

### P0 — `ccteam init -y` 不感知已有 meta-agent 项目

**症状**:host 上原本有 `~/projects/meta-cto/`(V0.4.0 handle=`cto` 老 layout,
state.json 含 $0.47 历史 cost + 真实 tmux session `ccteam-meta-cto`)。跑
`ccteam init -y` 后,wizard 平行新建了 `~/projects/meta/`,**没警告、没合并、
没迁移**。结果:

```
$ ccteam ls
SLUG                                     PHASE          STATE       COST   AGE
meta                                     pending        idle        $0.00  84s
meta-cto                                 pending        idle        $0.47  16898s
```

两个独立 state.json + 两个独立 tmux session 名,用户得手动决定保留哪个 +
把另一个 mv 走。

**需要的产品改动**:
1. `ccteam init -y` / `ccteam doctor --install-meta-agent` 先 scan `~/projects/meta*/`
2. 检测到 `meta-<handle>/` 且无 `~/projects/meta/` 时提示:
   ```
   found legacy meta-agent at ~/projects/meta-cto/ (V0.4.0 handle layout).
   V0.4.1 dropped the handle. run:
     ccteam doctor --migrate-meta-handle
   to mv ~/projects/meta-cto → ~/projects/meta (preserves state.json).
   ```
3. 检测到 `~/projects/meta/` 已存在时 short-circuit `already-present`,
   不要重写 CLAUDE.md / state.json

### P1 — 运行中 daemon 不 hot-reload 新 project

**症状**:`init -y` 在 18:28:57Z 创建 `~/projects/meta/`,但跑在 18:24:43Z
启动的 daemon 没监到。`tail /tmp/ccteam.log` 只见 `dev-ui-quality` 一条
event loop,没有 `meta`。`ccteam ls` 显示 meta 是因为它直接扫 filesystem,
但 orchestrator 内部 roster 仍只装 `dev-ui-quality`(故 `ccteam send meta "..."`
会 route 不到)。

修复:
- orchestrator tick 重 scan `~/projects/*/`,新发现的起 event loop
- 或 `ccteam new` / `ccteam init --install-meta-agent` 在 daemon 跑时发 USR1
- 或最轻:`ccteam status` 多一行 "N on disk / M loaded — restart needed to pick up X"

### P2 — Web 路由命名没 doc SoT

**症状**:verify prompt 写 `curl /api/health`,实际是 `/health`(`/api/health` 404)。
prompt 作者凭印象写错,暴露的是:web API 命名约定没集中文档。`/api/{slug}/...`
(actions)、`/api/v1/...`(data)、`/health`(loose root)、`/app/...`(SPA)、
`/assets/spa/...`(static)分布在 5 个 mod 里要 grep 才知道。

修复:`docs/interfaces.md` 加 §11.x "Web API 路由表"全列出;`ccteam status`
打一行 `web: /health · /app/ · /api/v1/*`。

### P2 — `ccteam init -y` output 没 next-steps hint

**症状**:`init -y` 末行是 `tmux session     ccteam-meta`,exit 0,但用户不知道
"下一步干嘛"。期望像 `cargo new` 那样:

```
✓ meta-agent ready. attach with:
    tmux attach -t ccteam-meta
✓ start the orchestrator:
    ccteam start &
✓ create your first project:
    ccteam new "build a foo cli"
```

### P2 — `ccteam send` 默认 first manual-trigger role 不可覆盖

**症状**:`ccteam send dev-ui-quality "..."` 命中 `spawn=Some("explorer")`
(workflow.yaml 第一个 `trigger: manual` role)。多 manual-trigger workflow
没法 send 时指定哪个 role。

修复:`ccteam send <slug> --role <role> "..."`,或 inbox markdown frontmatter
支持 `target_role:` 字段。

---

## Lessons(给我自己)

1. **永远不要 `rm -f ~/.ccteam/state/orchestrator.pid`** — 那是个安全锁。
   导致这一轮 deploy host 出现两个 orchestrator 并跑。正确做法是 `ccteam stop`
   等真退出,或者 `kill -9 <pid>` 后 *再* `rm` pidfile。
2. **`pkill -f "pattern"` 通过 ssh 容易杀掉 ssh 自己的 bash session** — 撞过一次。
   用具体 pid 优于 pattern。
3. **`ccteam start &` 后 ssh 退出会带走 child** — 必须 `nohup ... & disown` 才能
   留住。已撞过。
4. **远端测试用 release build 不要用 debug** — debug 90s 编译 + 7MB binary,
   release 60s 编译 + 2-7MB 但启动快。当然下次该 push prebuilt。
