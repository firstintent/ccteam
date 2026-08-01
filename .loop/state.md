# ccteam 当前状态(`.loop/state.md`)

> **本文件的家**:当前焦点 · 基线数字 · 人工门登记 · 未固化教训 · 流程速查。
> 维护者 = 规划(控制)会话,每版 ship / 每波收口时回填;**dev 会话只读**。
> 瘦身纪律:落盘前自问「下个 session 第一屏需要它吗?」——完成细节沉 `git log` 与
> `docs-local/versions/`(gitignored),教训固化进 `.loop/verify/README.md` / AGENTS.md 后此处只留指针。

## 当前焦点

- **版本线**:workspace **`0.9.12` 已发布**(2026-08-02,owner「把最新的积累在 dev 分支上的发 release」→ ship gate 全绿 → dev→main PR → merge commit → tag → release;登记下表人工门;一行史 = `.loop/history.md` v0.9.12 行,详档 = `docs-local/versions/v0-9-12/`)。上一发布 = v0.9.11(2026-07-29,main `efce0196`)。**gh 按机器分**:macOS 机 = `/opt/homebrew/bin/gh`(firstintent,repo+workflow scope);**Linux dev 机(ccd)= `~/.local/bin/gh`**(firstintent,scope 仅 `gist,read:org,repo` —— **无 `workflow`**,改 `.github/workflows/*` 须走 SSH push;git protocol 本就 ssh,普通推送不受影响)。
- **在做**:无 —— **v0.9.12 = 累积周期收口发布**(全程 owner 直驱、无卡):`e6fbef7`+`3be807b`(live session 继承 tenant project owner + 和解 main 合并)· `410647d` ACP 结局契约(`stopReason` 不再把失败洗成答案)· `1ce65b86`+`379cd2b2` MCP 传输统一 HTTP + 升级修复面 · `08aa865e`+`53074ff8`+`ffc86515` install 单一落点阶梯(治「三个 binary 三个真相」)· `b6634b26`+`0dcce1da`+`80e12f6e` 上下文口径三修(unknown≠0% / ACP status 快照落盘 / kimi 经其自报 `status` 命令拉真占用)· `18a79f04`+`00b622ab` 团队拓扑「模型·强度」列 + vendor 原文强度 · `4d223cf5`+`02c6d1b5`+`a0b714f9`+`13d9ace7`+`daef69b0` **spawn 调参轴打通**(按 spec category 认轴 → 每 vendor 真下发 → web 真菜单 → 三入口一致 → `meta.json` 持久化,五层一条线)。队列现势 = TD-SYNC-1 / A2A-W5 / FB-2 / P1-1 / P1-2 / TEST-MACOS-1 / ACP-LEDGER-1 / KIMI-UPSTREAM-1 / DEPLOY-DRIFT-1 + 候选 STATE-CULL-1 / A2A-OBS-1..5(V094 gated)。
- **下一版**:A2A 可观测性补丁(A2A-OBS-1..4,蒸馏自 kimi 委派复盘)或 owner 另点;v0.9.4(npm 分发)gated 不变。**周期纪律复归**:下个周期首个新提交即开 dev→main draft PR(AGENTS §五;v0.9.12 周期直到收口才开 PR,期间 `push[dev]` 零 CI 覆盖 —— `check.yml` 触发面 = `push[main]` + `pull_request[main]`)。

## 基线(口径与 env-flake 族见 `.loop/verify/README.md`;只增不减)

- 确定性口径 `make test-baseline`(`--lib --bins`)= **1757/0**(v0.9.12 收口,Linux dev 机(ccd)默认 shell 全绿实测,7 个 target;上口径 1708(v0.9.11)+49;干净环境仲裁 = v0.9.12 PR CI)。macOS 宿主两族(TMPDIR 形状 / UDS SUN_LEN)登记不变,修卡 TEST-MACOS-1,Linux 不受影响
- `make test`(workspace 除 web,**全 target**)= **120 bins / 2506 通过 / 0 失败**,exit 0(v0.9.12 收口本机实测)。⚠️ **计数纪律**:`make test 2>&1 | grep …` 后台跑会因 grep 块缓冲**丢尾段**(实锤:同一次跑先读到 60 bins/1383,重跑落盘完整文件是 120 bins/2506)—— 记基线数字一律**先落盘完整日志再 awk 汇总**,不要在管道里边跑边 grep
- `ccteam-web` 全量:**347 通过 + 3 红**(`cargo test -p ccteam-web --no-fail-fast`,34 bins;上口径 344+3)= `pty_ws_test` `ws_*` 已登记 env-flake 族(本机确有 live daemon 在跑,非回归)。**注**:`make test-web` 配方**没有** `--no-fail-fast`,`ws_*` 一红即中止余下 binary(本次只跑到 24 bins/280)—— 记全量数字请自己加 `--no-fail-fast` · vitest **566**(52 文件;上口径 532 + 调参轴两波 +34)· tsc(`npx tsc -b`)/ eslint 干净 · Playwright **7**(未重跑,沿用口径)
- clippy **0 warnings**(`-D warnings`,含 ccteam-web)· `cargo fmt --all -- --check` 干净

## 人工门(不许任何 agent 在任务内自决;签核 = 一次性授权,登记于此)

| 事项 | 状态 |
|---|---|
| **v0.9.12 发布(bump + ship gate 回填 + 开 dev→main PR + merge + tag + release.yml)** | **已签核消耗** —— owner 2026-08-02「把最新的积累在 dev 分支上的发 release」;版本号 = 规划按 0.9.x 节奏定 `0.9.12`(owner 未点名字面,循 v0.9.11 先例的补丁位累加)。执行链见「版本线」行。**含一次性治理写权**(dev 会话据此写 AGENTS.md §一 + `.loop/{state,history,backlog}.md` + `docs/`):常态不变,治理面写权归 Fable 5 规划会话,下次仍须停手申报。**常态不变:push main ≠ 发布,下个版本 tag 仍需 owner 显式发话** |
| **本机 dev daemon 换装重启(部署 `410647d5` binary = tenant `/status` ACL 修复上线)** | **已签核消耗** —— owner 2026-07-31 IM「拉最新代码…彻底修复」(rob 租户 `/status` 无 👥 直接子会话,web 拓扑可见):排查实锤病根 = **修复从未上线**,代码本身正确(见教训行);行动 = 410647d5 实体安装至 `~/.local/bin/ccteam` 与 `/data/.local/cargo/bin/ccteam`(替换断链软链与 Jul-2 旧拷贝)+ 脱离式脚本重启 daemon(旧映像备份 `/tmp/ccteam-old-running-backup` 可回滚,脚本+日志 = `~/.ccteam/redeploy-20260731.{sh,log}`)。范围仅本机 dev daemon;tag/发布常态门不变 |
| **v0.9.11 发布(merge PR #171 + tag + release.yml)** | **已消耗(v0.9.11 已发布 2026-07-29)** —— owner「合并pr,tag 和发release」;执行链:PR CI 三 job 绿(head `a01150e3`)→ ready → **merge commit** `efce0196`(非 squash,dev 已成 main 祖先、免和解可直接续用)→ main CI 绿 → annotated tag `v0.9.11` → release run 30427416169。常态不变:下个版本 tag 仍需 owner 显式发话 |
| **v0.9.11 团队页重设计开工(PRD 全默认拍板 + 版本号钦点 + 推 dev)** | **已签核消耗** —— owner 2026-07-29「确定,启动开发。提交推送到dev即可。pr已有 #171」= PRD `docs-local/versions/v0-9-11/prd-team-page.md` 待拍板五项全默认(v0.9.11 / roster+timeline 直删 / 全局 routing web 只读 / 起手卡文案入 repo / P2 面不做);授权范围 = 开发 + 推 dev(发布另见上行);同日 owner 追加「后选动工,派 subagent opus」= TEAM-7..10 三候选 + 收尾卡授权 |
| **v0.9.10 发布(merge PR #170 + tag + release.yml)** | **已消耗(v0.9.10 已发布 2026-07-26)** —— owner 深夜授权「验收通过后…直接tag + release,明早 github release 见二进制」;执行链全绿:merge `180e91b` → tag `v0.9.10` → release run 30218361955 success(四平台 tarball + SHA256SUMS,`/releases/latest` 已指向);常态不变:下个版本 tag 仍需 owner 显式发话 |
| **tag + 部署** | **已消耗(v0.9.8 已发布)** —— owner 2026-07-23「人肉测过了,打tag、发release」→ 正式 `v0.9.8` tag(main squash `ad1c7c2`)推送,release.yml 全绿(四平台 tarball + SHA256SUMS);`/releases/latest` → v0.9.8,全体用户经 `install.sh`/`ccteam update` 可拿到(v0.9.8 无 rc,owner 已先行人肉测)。上一次 = v0.9.7(`2922f7a`,rc 先行)。**常态不变:push main ≠ 发布,下个版本 tag 仍需 owner 显式发话** |
| V097(v0.9.7 daemon 重构 + update)W0 拍板 | **已签核消耗** —— owner 2026-07-22「install.sh 检测 systemctl…你来调度进入开发,提交 dev,发 PR」;废 systemd/launchd 先期拍板 + D1–D8 按 PRD v4 默认全「是」消耗(**含 D2 `daemon stop --force` SIGKILL 例外,仅 daemon 自身,agent session 零碰**);merge PR #165 = owner 2026-07-22「已经合并」;`825ae7d` squash 落 main |
| v0.9.6 compare 契约删除(REST `/compare`×2 + IM `/compare` + web tab) | **已签核消耗** —— owner 2026-07-21 会话拍板「compare 去掉,改会话内编排」,落 dev(T4) |
| v0.9.6 docs 写权一次性授权(kimi 改 usage/orchestration/tech-design/README) | **已签核消耗** —— owner 2026-07-21 指定 kimi 更新全局文档、fable5 review;仅本版有效,写权常态仍归规划会话 |
| v0.9.6 合 main | **已签核消耗** —— owner 2026-07-21「review 后合并 main,让 dev 和 main 保持一致」;fable5 review 三提交(3e6bca1/9c5f895/86b9788)后 ff 合并 |
| AGENTS §三 init 布局红线行澄清(注明用户可选 `.ccteam/routing.md`,init 不种) | **已消耗** —— 随 owner ship 9c5f895 的语义校准,非新增红线 |
| v0.9.4 动代码 | gated —— owner 暂缓(2026-07-17,v0.9.5 先行;v0.9.5 已于同日完成落 main,授权已消耗) |
| 分支治理 = dev + PR 攒版本(常态规则,非一次性) | **已生效** —— owner 2026-07-22:「后续新功能开发一律在 dev 分支开发,提交 PR;多个提交累计组成一个版本,owner 合并 PR 后复用 dev 重复」;取代旧 direct-on-main。**2026-07-24 owner 补充拍板:合并方式 = merge commit(非 squash)**(main 含完整历史、免每轮和解——v0.9.8/v0.9.9 两轮 squash 和解成本实证)+ 周期开始即开 draft PR(dev 推送借 PR 跑 CI);均已固化 AGENTS.md §五「分支与推送」 |
| 外部 Agent MCP 接入 Phase 1(研究稿 `docs-local/research/external-agent-mcp-symmetric-architecture.md` 待拍板 D1–D10) | **已签核消耗** —— owner 2026-07-23「实现这个需求」= 按稿内推荐默认拍板,授权范围仅 Phase 1(主 daemon WebUser MCP,tenant token 直用为 MVP);Phase 2(独立 MCP token)/3(卫星 relay)/4(多 Authority)未授权;8-tool wire schema 不变 |
| v0.9.9 需求决策委托 + 开工(全局 skill 库 PRD FREEZE 解锁) | **已签核消耗** —— owner 2026-07-24「review v0-9-9 版本需求→不恰当处由规划决策改良→完成开发→治理沉淀清理→提 PR,owner 合并」;规划决议落 `docs-local/versions/v0-9-9/decisions.md`(O1–O5 钉死 + ADJ-1 全局面 admin-only + ADJ-2 rm 防误删 + ADJ-3 并入复盘 P0-1 wait 240,**wire schema 不变、additive 字段**;复盘其余排 A2A-OBS 卡)。merge 权仍在 owner |
| AGENTS §三红线**表述**治理(GOV-CE-2:瘦身 + resume-by-sid 行并入 session 一等实体行,语义零变更) | **已签核消耗** —— owner 2026-07-26「批准」chat 提案(逐行判定 + 重写样例);落地 `f0c834c`。红线**增删**类变更 = **A/B/C/E 已签核消耗**(owner 2026-07-26「批A B C」+ 追加「E删除」,落地 `23d0cef`:新增 引擎零 LLM / daemon 无自主内容决策循环 / vendor 配置足迹唯一写,README 行迁家 §五.7);**D(never-execute 并入 #15)未批不动** |
| MCP `screenshot` 工具删除(wire 契约变更,8→7)+ 关联面全删扩令 | **已签核消耗** —— owner 2026-07-26 IM「删除 mcp__ccteam__screenshot,这个是 tmux 时代的遗留产物」= MCP 工具面(1ab85da);同日追加「screenshot 关联的一并删」= web `/screenshot` 路由 + core 渲染管线与四依赖 + IM `/screen` 全退役(614ed9b);§三「不解析终端输出」行措辞随令两次同步(语义不变,pane-snapshot 仍为只读路径);chat_send_file 同问评估 = 保留(IM 外发通道,报告 owner 无异议) |
| MCP 裸名发现别名(wire 契约变更,7→8) | **已签核消耗** —— owner 2026-07-26「改吧…opencode排到最后」→ 中途改令「纯别名把」「方便后续随时改」= 新增 `claude_codex_grok_kimi_opencode_status`(status 纯 alias,ffcf817);名字自 AgentVendor::ALL 派生测试锁死,后续改/删只动别名不动 status |
| MCP `session_spawn` `protocol` 参数删除(wire 契约变更:schema property 删除 + 传入即硬错) | **已签核消耗** —— owner 2026-07-26 IM「在 mcp 调用的时候删掉协议字段…每个 vendor 只选一种最佳的协议…这个字段用默认,没必要让调用方去传」;每 vendor 恰一通道(claude/codex=stream-json,grok/opencode/kimi=acp),先例对齐 host 参数(`HOST_SPAWN_PARAM_REMOVED`);落地 = MCP-CULL-3(codex 委派);REST/web 协议面(terminal/rmux admin beta)不在本令,退役另案 |
| MCP beacon 别名改名(wire 契约变更:`claude_codex_grok_kimi_opencode_status` → `grok_claude_codex_kimi`,旧名即删) | **已签核消耗** —— owner 2026-07-26 IM「必须优化 a」钦点新字面(grok 打头、去 `_status`、opencode 出列);仍为 status 纯别名(BEACON-1 决议「纯别名方便随时改」正是为此);落地 = MCP-DX-3(codex 委派) |
| spawn project 默认梯扩展(解析语义变更 + `config.yaml` 新键 `default_project` + `~/.ccteam/default_project` scratch 自动供给 + 响应 `project_source` 字段) | **已签核消耗** —— owner 2026-07-26 IM「必须优化 b」拍板服务端默认项目方向,「还有更好的方式吗」= 授权规划定形(五级梯 + configured 键 + lazy scratch);tenant 语义零变;落地 = MCP-DX-3 |
| status 返回瘦身(MCP status JSON 形变 + core `ProjectState` 退役字段 team/phase/tmux_session 全链清除) | **已签核消耗** —— owner 2026-07-26 IM「status 返回冗余…从架构多层通用性和对称性去修」+「省 token…不能变 token 刺客」+「其他省 token 也你来决定,功能优先其次省 token」;落地 = STATUS-SLIM-1(codex 委派,排 MCP-DX-3 后) |
| web ACL 收敛(§三 ACL 行「全局·运维面仅 admin」修订 + REST 契约 403→200 放开(status/hosts/skills/项目 MCP 注册/global-attach)+ tenant 自助 reset-token) | **已签核消耗** —— owner 2026-07-27「除了新增用户,其他的功能应该放开给普通用户…在web进行调整。权限的底层逻辑应该是用户token和绑定的项目」;落地 = WEB-ACL-1(`1025450`),红线行文随治理收口 commit 同步。边界判定(规划,owner 如要更宽另令):全局 bot 凭据 `/config/im` 保持 admin(身份对称 —— tenant 家 = `/me/im`,各人管各人凭据);terminal 协议 UI 门保持(冻结面非本令);登录链接卡 admin(= 他人 token 面) |
| 治病根优先于补丁(常态规则,非一次性) | **已生效** —— owner 2026-07-28:「从架构层的通用性来设计解决真正的病根,而不是补丁式的。把这条加入 agents.md」;已固化 AGENTS.md §五 总纲(两条判据:同形扫一遍 / 新入口自动被覆盖;并覆盖测试定性——「登记为 flake」常是病根未找到)。同日串用户扫荡为实锤来源 |
| ACP 结局契约修复开工 + 推 dev(owner 直驱,无卡) | **已签核消耗** —— owner 2026-07-30 IM「分析本机这个kimi会话为什么卡主,从ccteam架构层面分析。修复。不要打补丁而是分析kimi的适配和架构层面修复」+「有更新直接提交dev分支推送…如果没问题不要硬修,从架构通用性、对称性的用户体验考虑优化」+「推送」;落地 `410647d`(rebase 于 `3be807b`)。授权范围 = 代码面修复 + 推 dev;**不含** tag/发布/开 PR。owner 「不要硬修」被当作判据用于两处**主动不修**:kimi `failed→end_turn` 折叠(需解析 vendor 私有 log 布局)与 kimi `ctx —`(vendor 不上报,编数=造数据) |
| **开发会话日耗上限 15 USD / 自然日 + 自主连跑(常态规则,非一次性)** | **已生效** —— owner 2026-08-01:「每天日耗控制在15U以内。可以一直跑。不用问我。写入治理」;已固化 AGENTS.md §五.9。语义:预算内**不请示**、持续取活;逼近上限**减小规模**(缩 wave/少派 subagent/降档)而**不停工**;省额度不换质量(测试门与基线红线不动)。与产品面 `budgets.*.max_cost_usd_per_24h` 无关(那是 agent session 的自动停,本条约束开发会话自身花法) |
| 一次性治理写权授予 dev 会话(2026-08-01 MCP 传输统一) | **已签核消耗** —— owner「写入治理」(随日耗令);dev 会话据此写 AGENTS.md §五.9 + 本文件两行。**常态不变**:治理面写权归 Fable 5 规划会话(AGENTS §五),本次为 owner 点名例外,下次仍须停手申报 |
| 一次性治理写权授予 dev 会话(2026-07-30 ACP) | **已签核消耗** —— owner 2026-07-30「写治理」;dev 会话据此写 `.loop/state.md` + `.loop/backlog.md`(新卡 ACP-LEDGER-1 / KIMI-UPSTREAM-1)+ `docs/dev/tech-design.md` 指针行 |
| 改 AGENTS.md §三红线 / 降任何基线 / 改对外契约语义(REST `/api/v1` · MCP wire) | 须 owner 签核后才动 |

## 未固化教训

- **「构建成功 ≠ 已部署」:修复的验证必须核到运行中进程的 build sha**(2026-07-31 rob 租户 `/status` 实锤):「同一 bug 修过两次仍复现」的病根不在代码 —— 48bd3c81/e6fbef72 正确且测试绿,但从未接管活 daemon:①本机 `CARGO_TARGET_DIR=/data/.local/cargo-target` 重定向产物,部署软链 `~/.local/bin/ccteam → repo/target/release/ccteam` **生来断链**;②daemon(efce019,Jul-29 起)从未重启,`/proc/<pid>/exe` 指向已删旧映像;③PATH 前列还有 Jul-2 旧拷贝 `/data/.local/cargo/bin/ccteam` —— 三个 binary 三个真相。纪律:声称「已修复」前必对齐 `ccteam --version`(磁盘)与运行中 daemon 的 build(`/proc/<pid>/exe` 拷出来跑 `--version`);本机部署 = **实体拷贝**安装,禁软链构建产物。产品化防复发 = DEPLOY-DRIFT-1(daemon 外显 build sha + doctor 漂移告警)。
- **vendor 报告的 turn 结局是数据,静默降级为「成功」= 契约腐蚀**(`410647d` 实锤,建议升为 adapter 契约条目):ACP `session/prompt` 即使 turn 被拒绝/截断/取消,返回的仍是**成功的 JSON-RPC 响应**,`stopReason` 是协议里唯一说明结局的字段——而它在全仓从未被读(grep 只命中测试 fixture)。后果不止 UX:半截前言当最终答案落 `turns.jsonl`,委派 parent 被告知任务完成(`vendor_error:false`),即**账本与完成通知同时说谎**。修在共享 `acp/` 层(`AcpStopReason`)故新 ACP vendor 自动被覆盖(符合「新入口自动覆盖」判据);缺失字段必须判 clean(否则回归 grok/opencode)。**同形待扫**:凡「vendor 给了结局字段而我们只取内容」的消费点(claude `subtype`、codex `will_retry` 已处理;其余 adapter 待核)。
- **「turn 静默」判定必须锚真实起点,arm 在 submit 就会错怪排队者**(`410647d` 实锤,「同形扫一遍」判据的直接战果):`turn_started_at` 早已只认 `Started`/canonical `TurnStarted`,但 `watched_turn`(静默 watchdog)仍在 submit 无条件 arm ——**同一形状修了一半**,漏的那半让 kimi FIFO 里排队的 turn 5 分钟后收到「turn 静默,建议 /stop」,替前一个 turn 的静默背锅,正是用户读成 STUCK 的来源。教训:改「turn 是否在飞」的判定时,把所有读该状态的消费点一并列出(`turn_started_at` / `watched_turn` / `latest_activity` / `visible_events`),别只修触发点。
- **vendor 缺陷的诚实边界 = 不耦合它的私有布局**(kimi 0.29.x,owner「不要硬修」判据落地):kimi 把 `turn.ended reason=failed` 映射成 `stopReason:end_turn`,429 错误只进它自己的 RotatingFileSink 日志(stderr 只有 node warning,实测 2 行)——真相存在,但只在 `~/.kimi-code/sessions/wd_*/session_*/logs/` 这类**非契约面**。去解析它能立刻「修好」现象,代价是耦合上游随时可改的私有文件布局。判定:**不做**,写进适配器头注释 + 立 watch 卡(KIMI-UPSTREAM-1),诚实信号交给 watchdog(现已带 turn 时长 + 最后活动)。同理 `ctx —`:kimi ACP 面不上报 window/token(`usage_update`/`session_info_update` 只在其 schema、从不发出),编个百分比就是造数据 → 改为 `/status` 说明原因。grok/opencode/claude/codex 均正常上报,已审计。
- **by-sid 门后的测试必须真实例化会话**(v0.9.11 wave 修复实锤):`074e284f` 把 `/sessions/{sid}/*` 全量走 sid→project 解析(live map → meta.json 扫描)后,凡「凭空 sid 打门」的 fixture 确定性 404 —— ACL 判正确、fixture 是病根,修法 = 补真实 spawn(HTTP create 先行),不是松门;同形已扫全 `crates/ccteam-web/tests/` 无他例(auth_test 的 no-gateway 探针是锁死契约、非侥幸)。改 by-sid/归属类门时先跑 `sessions_api_test` 族。
- **fixture-only 验证限界**(ae24cb3 review 实锤):fake 只证「客户端实现了 fixture 定义的合同」,不证 vendor 真机行为——真机 grok idle interject 返 `queued` 且**自发 turn** 自答,代码嗅探的 `-32000` 臂是死代码,P0 丢答案由**真机冒烟**揭示(fake 恒绿)。新 vendor wire 行为面收口前上真机冒烟(与 A2A-OBS-3 manual-gate 同旨);冒烟脚本模式:纯 stdio JSON-RPC 直驱 vendor binary,留痕 idle/mid-turn 两态响应形状。
- **vendor 容量中断 = 委派链故障模式**(v0.9.9 FIX1 尾段 codex「model at capacity」,turn 断在门禁前):恢复路径 = `session_collect` 读账本中间记录 → 接手方按其结论收尾,不重做已完成的归因;工作品外部化(worktree/commit)= 会话可弃性。**产品侧主体已固化**(owner 复盘驱动,`2a2b38a`:TurnFailed/终态 Error 贯穿 DelegationSignal,通知冠 VENDOR ERROR = 修「假成功」;TurnStarted 刷 last_active = 消挤停误排);余量 = A2A-OBS-5/OBS-2 卡。恢复纪律候选固化 → verify/README 运行纪律。
- **「同机同红」stash 对照只证「非本 diff 所致」,不证「环境态」**(HERM-1 ① 误归因复盘:对照基线 origin/dev 当时可能已含回归;且「CI 绿」快照会过期)——跨环境同断言复现 = 优先判真回归;flake 归档必须记录首见 CI run 边界,定期复核。
- **委派卡「单 commit 含窄写回」⇒ 卡面 sha 无法自引用**(MCP-CULL-3 卡面 9638ce9 vs 实推 9c2a89e,amend 后漂移,规划 review 校正):委派 brief 应改为「实现 commit → 写回 commit 分离」(与规划自身的 loop: 收口同构),或规划收口时校正 sha。
- **规划自身教训**:backlog 批量卡片删除禁用 sed 范围盲切(v0.9.9 蒸馏时 sed 端点被此前 Edit 吃掉的卡头坑掉整段,靠 cp 备份 + Edit 精确重建恢复)—— 结构性 `.loop/` 编辑一律 Edit 工具 + 事后 `writeback.sh`。

## 流程速查

- **冷启动三读**:AGENTS.md(harness 自动加载)→ 本文件 → `.loop/backlog.md` 文件头 + 所取卡;代码按卡面坐标按需读,不做全仓扫描。
- **收口**:`cargo fmt --all` → 改动面门禁(地图 `.loop/verify/README.md`)→ `.loop/verify/writeback.sh`(队列结构校验)→ commit(英文)→ push `dev`(**main 不直推**;dev→main PR 攒版本,owner 合并)。
- **停止条件**:DoD 达成 → 收口报告 · 需越卡面授权 / 撞人工门 → 停手偏差申报 · 同一问题三次修不好 → 如实报告停(**禁伪造绿**)· 预算/上下文将尽 → 落盘暂停续跑。
