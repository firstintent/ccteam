# V0.4.1 Deploy 验证报告(host 192.168.1.19)

> 实操执行人:本 session 的 deploy-verify agent。
> 时间:2026-05-14T18:24Z–18:30Z(NAS 上 UTC 时钟)。
> 目的:在真机上跑通 V0.4.1 八项 UX 简化,记录新摩擦,作为 V0.4.2 候选输入。

---

## 1. 环境

| 项 | 值 |
|---|---|
| host | `rob@192.168.1.19`(NAS,WSL 同款 Linux 6.6.x 系)|
| deploy tree | `/vol4/1000/nasworkspace/ccteam` |
| pre-deploy HEAD | `f22b810`(滞后 main 两个 commit)|
| 拉取后 HEAD | **`892f5e1`** docs(v0.4.1): record ops frictions… |
| 编译 | `cargo build --release -p ccteam-cli` 1m31s ✓ |
| 启动方式 | `nohup ./target/release/ccteam start > /tmp/ccteam.log 2>&1 < /dev/null & disown` |
| daemon pid | `2663059` |
| web | `http://127.0.0.1:7331/` AUTH **disabled (loopback bind)** |
| 已存在 projects | `dev-ui-quality`($1.10,4h)、`meta-cto`($0.47,4h) |

预热观察:host 上有一条遗留 `mcp-serve` 进程(pid 2658093),不是 `ccteam start`,跟本次重启无关。pidfile 残留指 2630075(死进程),`ccteam stop` 干净清掉了——这条路径没回归。

---

## 2. 八项 UX 简化逐项验证

| # | 命令 | 期望 | 实测 | 结果 |
|---|------|------|------|------|
| 1 | `ccteam status` | daemon UP + projects + recent events + token | 全部显示;daemon healthy 29s heartbeat;projects 2 条(`dev-ui-quality` / `meta-cto`);5 条 recent events;web token = `/home/rob/.ccteam/web-token` | **pass** |
| 2 | `ccteam` (无参) | 显示 help,不 silent exit | 打了完整 `Usage: ccteam [COMMAND]` + 17 个子命令一段 description;exit 0 | **pass** |
| 3 | `ccteam show`(无 slug)| 列已知 slugs + hint | `dev-ui-quality` / `meta-cto`,带 `re-run as ccteam show <slug>` 提示;exit 0 | **pass** |
| 4 | `ccteam send dev-ui-quality "verify-test: …"` | 写 inbox + orchestrator 触发 spawn | inbox 文件 `msg-20260514T182744Z-001.md` 写入;3s 内 orchestrator log 出 `inbox message archived + routed spawn=Some("explorer")`;10s 后 latest job `7e97ddfa` state=working,intent 匹配 | **pass** |
| 5 | `ccteam spawn dev-ui-quality explorer "…"` | 写 spawn_requests + spawn | marker `explorer-1778783298278104.json` 写入;10s 后 latest job `3c619f74` state=working,intent 匹配 | **pass** |
| 6 | `ccteam attach --help` | 显示用法不真 attach | `Attach to a project's tmux session …`(简洁,合理);exit 0 | **pass** |
| 7 | `curl /api/health` | 返回 200 | **`/api/health` 返回 404** — 实际 health endpoint 是 `/health`(返回 `{"status":"ok","version":"0.4.0"}` HTTP 200);`/app/` SPA 200 | **observed**(prompt 路径错,实际 health 健康) |
| 8 | `ccteam init -y`(临时 dir) | 跑 wizard 装 skill/MCP/meta-agent | 三项都做:skill `already-present`、MCP registered、meta-agent 在 `/home/rob/projects/meta/` 创建(tmux=`ccteam-meta`)。F44 反向迁移 scan 干净;exit 0 | **pass(注意 side effect 见 §4)** |

**8/8 实质 pass**(第 7 项是 prompt 文档路径笔误,产品行为是正确的)。

---

## 3. 残留观察

| 项 | 数值 / 状态 |
|---|---|
| `~/projects/meta-rob/` | 不存在(handle 历史上是 `cto`,不是 `rob`,所以 prompt 设想的迁移场景不适用) |
| `~/projects/meta/` | **新增**(由本轮 §2.8 `ccteam init -y` 创建;tmux=`ccteam-meta`)|
| `~/projects/meta-cto/` | 仍在(老 layout;tmux=`ccteam-meta-cto`)|
| `~/projects/backup/` | 仍在(本次不动)|
| `~/.claude/jobs/` 累积 | 289 个 / 2.4M |
| orchestrator pidfile | `2663059`,与活跃进程匹配 ✓ |
| `ccteam ls` daemon 字段 | `daemon: up` |

---

## 4. 新发现的 V0.4.1 摩擦

### F-V0.4.1-A — `ccteam init -y` 不感知已有 meta-agent 项目

**P0 候选**。`~/projects/meta-cto/` 已存在(handle=`cto` 时代的老 meta);跑 `init -y` 后**平行创建** `~/projects/meta/`,**没警告、没合并、没迁移**。结果:`ccteam ls` 同时列出 `meta` 和 `meta-cto`,两个独立 state.json / 两个独立 tmux session 名(`ccteam-meta` vs `ccteam-meta-cto`),用户得手动决定保留哪个 + 把另一个 `ccteam doctor` 不到的目录 mv 走。

具体修复方向:
- `ccteam init -y` / `ccteam doctor --install-meta-agent` 先 scan `~/projects/meta*/`
- 检测到 `meta-<handle>/` 且无 `~/projects/meta/` 时,提示用户:
  ```
  found legacy meta-agent at ~/projects/meta-cto/ (V0.4.0 handle layout).
  V0.4.1 dropped the handle. run:
    ccteam doctor --migrate-meta-handle
  to mv ~/projects/meta-cto → ~/projects/meta (preserves state.json + history).
  or pass --keep-legacy to skip.
  ```
- 检测到 `~/projects/meta/` 已存在时,**不要**再次 bootstrap;直接 short-circuit `already-present`。

### F-V0.4.1-B — 运行中 daemon 不 hot-reload 新 project

**P1 候选**。`init -y` 在 18:28:57Z 写完 `~/projects/meta/.ccteam/state.json`,但跑在 18:24:43Z 启动的 daemon **没监到**:`tail /tmp/ccteam.log` 只见 `dev-ui-quality` 一条 event loop,没有 `meta` 也没有 `meta-cto`。`ccteam ls` 能看到 meta 是因为它直接 enumerate filesystem,但 orchestrator 内部 roster 仍只装 `dev-ui-quality`(故 `meta` 上发 `ccteam send meta "..."` 会 ENOENT route)。

具体修复方向:
- orchestrator tick 增加一步:重 scan `~/projects/*/`,新发现的 project 起 event loop(已有 watch 链路就跳)。代价是 tick 慢,但 P0(1m)级别可接受
- 或更轻:`ccteam new` / `ccteam init --install-meta-agent` 在创建完后**主动**给 daemon 发 USR1(roster reload signal)
- 或最轻:`ccteam status` 一行提示 "N projects on disk, M loaded in daemon — restart needed to pick up X"

### F-V0.4.1-C — `/api/health` 不是 endpoint(prompt 笔误暴露 doc 漂)

**P2**。verify prompt 写 `curl /api/health`,实际 endpoint 是 `/health`。这是 prompt 作者打错,但暴露的真问题:health endpoint 的命名没有 doc SoT,各处都得 grep code 确认。`docs/interfaces.md` §11 / §12 应有 web 路由表 SoT。

具体修复方向:
- `docs/interfaces.md` 加 §11.x "Web API 路由表":`/health`、`/api/{slug}/...`、`/app/...`、`/assets/spa/...`、`/api/v1/...` 全列出
- `ccteam status` 多打一行 `web endpoints: /health (HTTP), /app/ (SPA), /api/v1/* (data)`

### F-V0.4.1-D — `ccteam init -y` 完整 output 没指引 next step

**P2**。`init -y` 跑完后 stdout 末尾是 `tmux session     ccteam-meta`,exit 0,但用户不知道 "我下一步该做啥"。期望:

```
✓ meta-agent ready. attach with:
    tmux attach -t ccteam-meta
✓ start the orchestrator:
    ccteam start &
✓ create your first project:
    ccteam new "build a foo cli"
```

跟 `git init` / `cargo new` 一致的 "next steps" hint pattern。

### F-V0.4.1-E — `ccteam send` 路由 dev-ui-quality 但写错 role 优先级

**P2 observed,非回归**。`ccteam send dev-ui-quality "..."` 命中 `spawn=Some("explorer")`(workflow.yaml 第一个 manual-trigger role),但项目当前 workflow 的语义可能是 `fixer` 该响应。这是 V0.4.1 的设计选择(`first workflow trigger: manual role`),send 时没法显式选 role。

具体修复方向(已部分预见):
- `ccteam send <slug> --role <role> "..."`(F-V0.4.1-E 提议)
- inbox markdown frontmatter 写 `target_role:`(orchestrator 优先尊重)

---

## 5. 主仓 commit 计划

本报告会随 ops-frictions.md 一起追加新摩擦 entries,然后单条 commit + push。无 hotfix 候选(8/8 简化都 pass,新发现都是 UX 漏洞不是回归)。

## 6. V0.4.2 候选小结

按优先级:

1. **`ccteam init` 感知已有 meta-agent**(F-V0.4.1-A) — P0;影响每个老用户升级
2. **Daemon hot-reload roster**(F-V0.4.1-B) — P1;影响新建项目的"首次能用"路径
3. **interfaces.md 加 web 路由表**(F-V0.4.1-C) — P2;一次性补 doc
4. **`init -y` next-steps hint**(F-V0.4.1-D) — P2;新手 UX
5. **`ccteam send --role <role>`**(F-V0.4.1-E) — P2;多 manual-trigger 项目时痛
