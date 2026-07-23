# ccteam 当前状态(`.loop/state.md`)

> **本文件的家**:当前焦点 · 基线数字 · 人工门登记 · 未固化教训 · 流程速查。
> 维护者 = 规划(控制)会话,每版 ship / 每波收口时回填;**dev 会话只读**。
> 瘦身纪律:落盘前自问「下个 session 第一屏需要它吗?」——完成细节沉 `git log` 与
> `docs-local/versions/`(gitignored),教训固化进 `.loop/verify/README.md` / AGENTS.md 后此处只留指针。

## 当前焦点

- **版本线**:workspace **`0.9.9` 已集成落 `dev`**(owner 2026-07-24 委托「review 需求→决策改良→开发→治理→PR」;决议 = `docs-local/versions/v0-9-9/decisions.md`,内容一行史 = `.loop/history.md` v0.9.9 行),**dev→main PR 已开待 owner 合并**(合并 = 版本落 main;tag/部署仍 HELD)。上一版 v0.9.8 已发布(`ad1c7c2` + tag,`/releases/latest`)。
- **在做**:v0.9.9 等 owner merge;**PR CI 三 job 首跑 = P2-1 验收判据**(test job 新增,绿后 P2-1 卡转完成)。队列现势卡 = V099-SHIP(收尾中)/ A2A-W5 / FB-2 / P1-1/2 / P2-1 + 下一版候选 A2A-OBS-1..4(V094 gated)。
- **下一版**:A2A 可观测性补丁(A2A-OBS-1..4,蒸馏自 kimi 委派复盘)或 owner 另点;v0.9.4(npm 分发)gated 不变。

## 基线(口径与 env-flake 族见 `.loop/verify/README.md`;只增不减)

- 确定性口径 `make test-baseline`(`--lib --bins`,41c6569 修正:补上 binary-only `ccteam-cli` 的覆盖盲区)= **1650/0**(1623 + v0.9.9 批 +24 + FIX1 消红 +1 + P0 双修 +2;2026-07-24 规划于集成 worktree 实测,ship 点 `2a2b38a`)
- `ccteam-web` 全量:lib 137 + 集成套全绿,唯 `pty_ws_test` 3 个 `ws_*` 红 = 已登记 env-flake 族(live-daemon 宿主,**与 v0.9.8 实测同三只、main=dev 同红不变**,非回归)· vitest **417**(上轮记录 395;#166/#167 窗口 +11 未及回填,V099-W5 +11)· tsc/eslint 干净 · Playwright **7**(未重跑,沿用口径)
- clippy **0 warnings**(`-D warnings`,含 ccteam-web)· `cargo fmt --all -- --check` 干净

## 人工门(不许任何 agent 在任务内自决;签核 = 一次性授权,登记于此)

| 事项 | 状态 |
|---|---|
| **tag + 部署** | **已消耗(v0.9.8 已发布)** —— owner 2026-07-23「人肉测过了,打tag、发release」→ 正式 `v0.9.8` tag(main squash `ad1c7c2`)推送,release.yml 全绿(四平台 tarball + SHA256SUMS);`/releases/latest` → v0.9.8,全体用户经 `install.sh`/`ccteam update` 可拿到(v0.9.8 无 rc,owner 已先行人肉测)。上一次 = v0.9.7(`2922f7a`,rc 先行)。**常态不变:push main ≠ 发布,下个版本 tag 仍需 owner 显式发话** |
| V097(v0.9.7 daemon 重构 + update)W0 拍板 | **已签核消耗** —— owner 2026-07-22「install.sh 检测 systemctl…你来调度进入开发,提交 dev,发 PR」;废 systemd/launchd 先期拍板 + D1–D8 按 PRD v4 默认全「是」消耗(**含 D2 `daemon stop --force` SIGKILL 例外,仅 daemon 自身,agent session 零碰**);merge PR #165 = owner 2026-07-22「已经合并」;`825ae7d` squash 落 main |
| v0.9.6 compare 契约删除(REST `/compare`×2 + IM `/compare` + web tab) | **已签核消耗** —— owner 2026-07-21 会话拍板「compare 去掉,改会话内编排」,落 dev(T4) |
| v0.9.6 docs 写权一次性授权(kimi 改 usage/orchestration/tech-design/README) | **已签核消耗** —— owner 2026-07-21 指定 kimi 更新全局文档、fable5 review;仅本版有效,写权常态仍归规划会话 |
| v0.9.6 合 main | **已签核消耗** —— owner 2026-07-21「review 后合并 main,让 dev 和 main 保持一致」;fable5 review 三提交(3e6bca1/9c5f895/86b9788)后 ff 合并 |
| AGENTS §三 init 布局红线行澄清(注明用户可选 `.ccteam/routing.md`,init 不种) | **已消耗** —— 随 owner ship 9c5f895 的语义校准,非新增红线 |
| v0.9.4 动代码 | gated —— owner 暂缓(2026-07-17,v0.9.5 先行;v0.9.5 已于同日完成落 main,授权已消耗) |
| 分支治理 = dev + PR 攒版本(常态规则,非一次性) | **已生效** —— owner 2026-07-22:「后续新功能开发一律在 dev 分支开发,提交 PR;多个提交累计组成一个版本,owner 合并 PR 后复用 dev 重复」;取代旧 direct-on-main,已固化 AGENTS.md §五「分支与推送」 |
| 外部 Agent MCP 接入 Phase 1(研究稿 `docs-local/research/external-agent-mcp-symmetric-architecture.md` 待拍板 D1–D10) | **已签核消耗** —— owner 2026-07-23「实现这个需求」= 按稿内推荐默认拍板,授权范围仅 Phase 1(主 daemon WebUser MCP,tenant token 直用为 MVP);Phase 2(独立 MCP token)/3(卫星 relay)/4(多 Authority)未授权;8-tool wire schema 不变 |
| v0.9.9 需求决策委托 + 开工(全局 skill 库 PRD FREEZE 解锁) | **已签核消耗** —— owner 2026-07-24「review v0-9-9 版本需求→不恰当处由规划决策改良→完成开发→治理沉淀清理→提 PR,owner 合并」;规划决议落 `docs-local/versions/v0-9-9/decisions.md`(O1–O5 钉死 + ADJ-1 全局面 admin-only + ADJ-2 rm 防误删 + ADJ-3 并入复盘 P0-1 wait 240,**wire schema 不变、additive 字段**;复盘其余排 A2A-OBS 卡)。merge 权仍在 owner |
| 改 AGENTS.md §三红线 / 降任何基线 / 改对外契约语义(REST `/api/v1` · MCP wire) | 须 owner 签核后才动 |

## 未固化教训

- **vendor 容量中断 = 委派链故障模式**(v0.9.9 FIX1 尾段 codex「model at capacity」,turn 断在门禁前):恢复路径 = `session_collect` 读账本中间记录 → 接手方按其结论收尾,不重做已完成的归因;工作品外部化(worktree/commit)= 会话可弃性。**产品侧主体已固化**(owner 复盘驱动,`2a2b38a`:TurnFailed/终态 Error 贯穿 DelegationSignal,通知冠 VENDOR ERROR = 修「假成功」;TurnStarted 刷 last_active = 消挤停误排);余量 = A2A-OBS-5/OBS-2 卡。恢复纪律候选固化 → verify/README 运行纪律。
- **「main 烂测」第二例**(web_chat_newproject,#167 窗口带入,CI 不跑测试漏网):已由 FIX1 修(产品 bug)+ P2-1 CI test job 堵;**PR CI 首跑绿后本行移走**。
- **规划自身教训**:backlog 批量卡片删除禁用 sed 范围盲切(v0.9.9 蒸馏时 sed 端点被此前 Edit 吃掉的卡头坑掉整段,靠 cp 备份 + Edit 精确重建恢复)—— 结构性 `.loop/` 编辑一律 Edit 工具 + 事后 `writeback.sh`。

## 流程速查

- **冷启动三读**:AGENTS.md(harness 自动加载)→ 本文件 → `.loop/backlog.md` 文件头 + 所取卡;代码按卡面坐标按需读,不做全仓扫描。
- **收口**:`cargo fmt --all` → 改动面门禁(地图 `.loop/verify/README.md`)→ `.loop/verify/writeback.sh`(队列结构校验)→ commit(英文)→ push `dev`(**main 不直推**;dev→main PR 攒版本,owner 合并)。
- **停止条件**:DoD 达成 → 收口报告 · 需越卡面授权 / 撞人工门 → 停手偏差申报 · 同一问题三次修不好 → 如实报告停(**禁伪造绿**)· 预算/上下文将尽 → 落盘暂停续跑。
