# qa-autoloop — 4-stage QA automation template

> 自动找 bug → 自动改 bug → 自动 ship 修复。第一手验证项目是 dex-ui;
> 这份模板把所有 dex-ui 特定值抽到 `config.json`,任何项目改一份 JSON
> 就能跑。

## 这是什么

一个 ccteam workflow,跑 4 个 agent 形成闭环 QA pipeline:

```
   ┌─────────┐   drop marker   ┌────────┐   drop marker   ┌───────┐
   │ planner │ ──────────────▶ │ tester │ ──────────────▶ │ fixer │
   └─────────┘  triggers/      └────────┘  triggers/      └───────┘
   (扫覆盖空白    tester/        (跑场景       fixer/         (修最高
    生成新场景)                  开 issue)                    P 的 open
                                                            issue)
                                                                │
                                                                │ drop marker
                                                                ▼
                                ┌──────────┐  triggers/
                                │ releaser │ ◀──── releaser/
                                └──────────┘
                                 (review / merge /
                                  deploy / 验收 /
                                  关闭 issue / 重置
                                  backlog → tester)
```

四个 agent 各管一段:

- **planner** — 扫前端 / 后端功能地图,对照 `.ccteam/backlog/`,补 10-20 个新 pending 场景
- **tester** — 取 backlog 里 priority 最高的 pending 场景,跑测试(框架由 `frontend_test.kind` / `backend_test.kind` 决定),失败开 P1/P2 issue,通过 + 标记测过
- **fixer** — 取 `.ccteam/issues/` 里 priority 最高的 open issue,checkout fix 分支、写代码、push、开 PR、请求 review。**`track=backend` 的 issue 跳过**(后端 bug 由人工处理)。重试上限 3 次
- **releaser** — 监控 PR review 状态,approve 后 squash-merge、触发 staging 部署、跑 acceptance 验证、关闭 issue、把对应 backlog 场景重置为 pending(下次 tester 复验)。**绝不写 production**

## 何时用这个模板

✅ **适合**:
- 中型前端项目(SPA / Next.js / Vite / Vue / ...)
- 有 GitHub repo + staging 部署 + 一个 reviewer(Copilot / 自研 / 人)
- 想要"提了 PR 就自己跑过验收并关 issue"的全闭环
- 接受 ccteam 24×7 跑 agent 的成本(`max_cost_usd_per_24h` 由项目自配)

❌ **不适合**:
- 项目还没成型 / 没测试覆盖思路(先用 `ccteam new` 默认 explorer-only workflow)
- 没有 staging deploy(可以但 releaser 退化成"merge + skip 验收";考虑 `deploy.kind = "none"`)
- 高度合规 / 必须人工 approve 每个 merge(把 `review.reviewer = "human"`,但 releaser 仍会自动 merge 已 approved 的 PR,真要禁用直接把 releaser 的 trigger 改 manual)

## 接入步骤

### 1. 复制模板到项目

```bash
cd /path/to/your-project
mkdir -p .ccteam .claude/agents
cp <ccteam-repo>/workflows/qa-autoloop/workflow.yaml       .ccteam/workflow.yaml
cp <ccteam-repo>/workflows/qa-autoloop/config.example.json .ccteam/config.json
cp -r <ccteam-repo>/workflows/qa-autoloop/agents/*.md      .claude/agents/
cp -r <ccteam-repo>/workflows/qa-autoloop/rules            .ccteam/rules
```

### 2. 填 `.ccteam/config.json`

照 `config.example.json` 把所有 `TODO-*` 字段换成项目实际值:
- `name`、`local_path`
- `github.{owner,repo,fix_base_branch,fix_branch_prefix}`
- `deploy.{kind,project_id,staging_domain,...}` —— 若无 staging deploy,`deploy.kind = "none"`
- `test.{staging_url,production_url,active_env}`
- `frontend_test.{kind,test_root}` —— 若是纯后端,`frontend_test.kind = "none"`
- `backend_test.{kind,cli,api_url,env}` —— 若无后端测试,`backend_test.kind = "none"`
- `review.{reviewer,approve_strategy}`

### 3. 确保项目 `.gitignore` 包含 `.ccteam/`

`.ccteam/` 是本地 orchestration state,**不应入 git**。

### 4. 在项目根放一份 `.env`

```env
GH_TOKEN=ghp_...                # fixer / releaser 用
VERCEL_TOKEN=...                # 若 deploy.kind = vercel
# ... 其它项目特定的密钥(测试钱包、API key、etc.)
```

`.env` 也应在 `.gitignore`。

### 5. 注册项目到 ccteam 并启动

```bash
ccteam init --slug <your-slug> --here     # 或 ccteam new <slug>
ccteam start                              # 在一个长 session / tmux / systemd 里跑
```

### 6. 第一次激活 — 手动 spawn planner 填 backlog

```bash
ccteam spawn <your-slug> planner
```

planner 跑完后会写 N 个 pending 场景到 `.ccteam/backlog/`,并 drop 一个 marker 到
`.ccteam/triggers/tester/` 唤醒 tester。从此闭环开始自转。

### 7. 在 web 面板看

`http://<host>:7331` 看 4 个 agent 的状态、issue / PR / backlog 计数、cost trend、live events。

## 架构要点(为什么这么设计)

### 两层目录 — markers 和 tracking files 分开

`.ccteam/` 下有两类目录:

| 类型 | 目录 | 谁写 | 谁监听 |
|---|---|---|---|
| **Domain state**(实体)| `issues/` `prs/` `backlog/` `acceptance/` | 任何 agent 自由 mutate | 没人监听 |
| **Triggers**(事件)| `triggers/tester/` `triggers/fixer/` `triggers/releaser/` | 上游 agent drop marker | inotify 监听对应 agent |

**为什么不合一层**:agent 在监听目录写文件会触发 inotify Modify → 自激,4h 烧出 80 个 stale-spawn(已实测踩过)。两层分离是 self-trigger fix。

### "永远不要写文件到自己监听的目录"

每个 agent 都有这条铁律。下游联动靠 drop marker 到下游的 triggers/ 目录,不靠直接 spawn / RPC。

### Markers 是瞬时,不是 queue

每个 agent 开头读 markers、归档到 `.ccteam/triggers.archived/<role>/`、然后从 tracking dir 全表扫挑活。markers 只是"醒一下"的信号,真正的"接下来干啥"靠 status 字段判断。这样即使 marker 漏掉 / 重复 / 内容错乱,agent 也会做对的事(只要 tracking 文件状态对)。

### Track = frontend / backend

每个 issue / scenario 有 `track` 字段:
- `frontend` → tester 跑 frontend_test,fixer 自动改
- `backend` → tester 跑 backend_test,fixer **跳过**(由人工修)

这样后端 bug 进 issue 簿但不会被自动改坏。

## 红线(不要踩)

1. **Production 写禁止**:tester / releaser 只跑 `test.staging_url`,绝不对 `test.production_url` 跑任何东西。
2. **自激防护**:agent 永远不要写文件到自己的 `triggers/<role>/`。读完归档。
3. **fix_attempts >= 3 → needs-human**:不能无限重试,免得 cost 烧穿。
4. **`.ccteam/` 不入 git**:它是本地 orchestration state。
5. **不改 `.github/workflows/`**:fixer 禁止动 CI 配置。
6. **不 `git push --force`**:任何分支。

## 该模板的下一步演进(v0.5+)

- B 选项:`ccteam new <slug> --template qa-autoloop` 一键拷贝(目前手动 cp)
- C 选项:作为 `ccteam team` plugin 发布到 marketplace,`ccteam doctor --install-team qa-autoloop` 安装
- 用 `Trigger::Watch where status=...` 内容感知触发,去掉 marker dir(需 orchestrator 改造)
- `deploy.kind = "netlify"` / `"github-pages"` / `"cloudflare-pages"` 等 provider
- planner 加 schedule trigger(目前是 manual)
