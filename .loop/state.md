# ccteam 当前状态(`.loop/state.md`)

> **本文件的家**:当前焦点 · 基线数字 · 人工门登记 · 未固化教训 · 流程速查。
> 维护者 = 规划(控制)会话,每版 ship / 每波收口时回填;**dev 会话只读**。
> 瘦身纪律:落盘前自问「下个 session 第一屏需要它吗?」——完成细节沉 `git log` 与
> `docs-local/versions/`(gitignored),教训固化进 `.loop/verify/README.md` / AGENTS.md 后此处只留指针。

## 当前焦点

- **版本线**:workspace **`0.9.10` 已发布**(2026-07-26,owner 深夜授权规划全链执行,登记下表人工门;一行史 = `.loop/history.md` v0.9.10 行,详档 = `docs-local/versions/v0-9-10/`)。**`ae24cb3` 来源已确认 = owner 直驱 codex s131**(无卡口头令,活跃消息 vendor 注入):规划全量 review + **真机 grok 冒烟**发现 P0(完成边竞态丢答案:真机 idle interject 返 `queued` 并自发 turn 自答、`-32000` 降级臂死代码、无主 chunks 被静默丢弃)+ P1(Started 后陈旧 `turn_started_at` 不重盖)等 5 项 → codex s131 修复 **`f3ea8bf`**(vendor 自发 turn 合成收口 `capture_vendor_started_turns`、Started 无条件重盖时钟、bg 拒 Queue、tui 注释、去重复 override)已验收(红→绿在案:`completion_edge_interject_surfaces_vendor_self_started_turn` / `started_submission_refreshes_stale_working_start`)。**最近发布 = v0.9.10**(main `180e91b` + tag;上一 tag = v0.9.8,v0.9.9 合 main 未 tag)。**本机 gh = `/opt/homebrew/bin/gh`**(firstintent,repo+workflow scope;旧 `~/.local/bin/gh` 记载作废)。
- **在做**:无 —— **v0.9.10 已发布**(2026-07-26:治理提交 `9934218` → PR #170 CI 三 job 绿(干净环境仲裁 1687/0 实证)→ merge `180e91b` → tag `v0.9.10` → release run 30218361955 success,四平台 tarball + SHA256SUMS,`/releases/latest` 已指向;本机 `target/debug` 已按 owner 令清理,daemon 跑 `target/release` 不受影响)。队列现势 = TD-SYNC-1 / A2A-W5 / FB-2 / P1-1 / P1-2 / TEST-MACOS-1 + 候选 STATE-CULL-1 / A2A-OBS-1..5(V094 gated)。
- **下一版**:A2A 可观测性补丁(A2A-OBS-1..4,蒸馏自 kimi 委派复盘)或 owner 另点;v0.9.4(npm 分发)gated 不变。

## 基线(口径与 env-flake 族见 `.loop/verify/README.md`;只增不减)

- 确定性口径 `make test-baseline`(`--lib --bins`)= **1687/0 预期(干净环境;仲裁 = PR #170 CI)**(1683 + `f3ea8bf` +4:gateway Started 重盖 / translate 合成 turn ×2 / bg 拒 Queue)。本机默认 shell 实测 **1686 绿 + 1 红** = `roles::list_library_skills…` = **macOS TMPDIR 形状族**(`fs::canonicalize` /var→/private/var vs 字面断言;先于 `ae24cb3`、Linux CI 绿;TMPDIR 已 canonical 的会话本机也全绿——此前「本机全绿」与新会话红并存的成因;已登记 verify README「macOS 宿主两族」+ 修卡 TEST-MACOS-1)
- `ccteam-web` 全量:lib + 集成套全绿,唯 `pty_ws_test` 3 个 `ws_*` 红 = 已登记 env-flake 族(live-daemon 宿主,非回归)· vitest **436**(43 文件;上口径 422 + ACCESS-IA-1 +12 + WEB-ACL-1 +2)· tsc/eslint 干净 · Playwright **7**(未重跑,沿用口径)
- clippy **0 warnings**(`-D warnings`,含 ccteam-web)· `cargo fmt --all -- --check` 干净

## 人工门(不许任何 agent 在任务内自决;签核 = 一次性授权,登记于此)

| 事项 | 状态 |
|---|---|
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
| 改 AGENTS.md §三红线 / 降任何基线 / 改对外契约语义(REST `/api/v1` · MCP wire) | 须 owner 签核后才动 |

## 未固化教训

- **fixture-only 验证限界**(ae24cb3 review 实锤):fake 只证「客户端实现了 fixture 定义的合同」,不证 vendor 真机行为——真机 grok idle interject 返 `queued` 且**自发 turn** 自答,代码嗅探的 `-32000` 臂是死代码,P0 丢答案由**真机冒烟**揭示(fake 恒绿)。新 vendor wire 行为面收口前上真机冒烟(与 A2A-OBS-3 manual-gate 同旨);冒烟脚本模式:纯 stdio JSON-RPC 直驱 vendor binary,留痕 idle/mid-turn 两态响应形状。
- **vendor 容量中断 = 委派链故障模式**(v0.9.9 FIX1 尾段 codex「model at capacity」,turn 断在门禁前):恢复路径 = `session_collect` 读账本中间记录 → 接手方按其结论收尾,不重做已完成的归因;工作品外部化(worktree/commit)= 会话可弃性。**产品侧主体已固化**(owner 复盘驱动,`2a2b38a`:TurnFailed/终态 Error 贯穿 DelegationSignal,通知冠 VENDOR ERROR = 修「假成功」;TurnStarted 刷 last_active = 消挤停误排);余量 = A2A-OBS-5/OBS-2 卡。恢复纪律候选固化 → verify/README 运行纪律。
- **「同机同红」stash 对照只证「非本 diff 所致」,不证「环境态」**(HERM-1 ① 误归因复盘:对照基线 origin/dev 当时可能已含回归;且「CI 绿」快照会过期)——跨环境同断言复现 = 优先判真回归;flake 归档必须记录首见 CI run 边界,定期复核。
- **委派卡「单 commit 含窄写回」⇒ 卡面 sha 无法自引用**(MCP-CULL-3 卡面 9638ce9 vs 实推 9c2a89e,amend 后漂移,规划 review 校正):委派 brief 应改为「实现 commit → 写回 commit 分离」(与规划自身的 loop: 收口同构),或规划收口时校正 sha。
- **规划自身教训**:backlog 批量卡片删除禁用 sed 范围盲切(v0.9.9 蒸馏时 sed 端点被此前 Edit 吃掉的卡头坑掉整段,靠 cp 备份 + Edit 精确重建恢复)—— 结构性 `.loop/` 编辑一律 Edit 工具 + 事后 `writeback.sh`。

## 流程速查

- **冷启动三读**:AGENTS.md(harness 自动加载)→ 本文件 → `.loop/backlog.md` 文件头 + 所取卡;代码按卡面坐标按需读,不做全仓扫描。
- **收口**:`cargo fmt --all` → 改动面门禁(地图 `.loop/verify/README.md`)→ `.loop/verify/writeback.sh`(队列结构校验)→ commit(英文)→ push `dev`(**main 不直推**;dev→main PR 攒版本,owner 合并)。
- **停止条件**:DoD 达成 → 收口报告 · 需越卡面授权 / 撞人工门 → 停手偏差申报 · 同一问题三次修不好 → 如实报告停(**禁伪造绿**)· 预算/上下文将尽 → 落盘暂停续跑。
