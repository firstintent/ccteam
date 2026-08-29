# ccteam 工作列表(backlog · 跨 harness 共享 · 版本化迭代)

> **任务队列唯一来源**。本仓按**版本迭代**排卡:大改 = 版本波(doc-first PRD 住
> `docs-local/versions/v0-x-y/`,owner 拍板后由规划拆成 wave 卡进本文件);小/中改 = 独立卡(owner 直驱)。
> 任何入口(Claude Code / Codex / 自由一句话指名某卡)消费同一份:按本文件头协议 + 该卡 DoD 执行,完成同样回写。
> **共守**(与入口无关):AGENTS.md §三红线 · 门禁唯一来源 = 根 Makefile(地图 `.loop/verify/README.md`)·
> 每波基线只增不减 · fail-fast 无兜底 · 跨会话/跨机接力只认已提交物。
> **取活/回写**:按优先级取「待排」卡;**并行开工须不同冲突域**(同域串行)+ 各自独立 worktree(AGENTS.md §五);
> 开工改状态「进行中(入口·YYYY-MM-DD)」;完成改「完成(<7位hex sha>)」;阻塞标「阻塞(原因)」;等 owner 决策 =「gated(事项)」。
> **窄写回**:dev 会话只许改**自己所取卡**的状态行 + 追加两段(**验证** / **偏差**,偏差段末可附「经验:」行供规划蒸馏);
> 文件头、他卡、卡面规格、`.loop/` 其余文件 = 规划(控制)会话(Fable 5)专属 —— 执法 = 声明 + 复核,无脚本硬防护,越界靠 review 抓;收口必跑 `.loop/verify/writeback.sh`(无参数,队列结构校验)。
> **冲突域约定**:首段 = **路径前缀**(如 `crates/ccteam-harness`),前缀重叠即同域须串行。
> **偏差申报**:完成 DoD 必须越出卡面授权时**停手**,状态改阻塞,偏差段写清矛盾 + **最窄解锁提议**,等裁决;
> 裁决只授权提议字面,不隐性扩 scope。状态行用 ASCII 冒号 `:`(守卫按此校验)。

## 当前卡

> **v0.10.5 已 ship(2026-08-29,owner「ship」;合并 v0.10.4 周期 + v0.10.5 波)**。本段只持现势:完成卡已蒸馏进 `.loop/history.md` 一行 + 原文归档 `docs-local/versions/v0-10-5/backlog-archive.md`(gitignored);下一波未拍板,待排卡按优先级取(候选:ACP-DISCONNECT-1 / LIFE-2 / DSH2-CHIP / DSH1-MODE-UI / LEDGER-1 / TD-SYNC-1)。需求 SoT = `docs-local/versions/v0-10-5/prd.md`。 **UI 材料红线(owner 令 2026-08-21)**:组件只用 `@deepseek-ai/dsh-client-ui-primitives`,token 只引语义层 `--dsw-alias-*`/`--dsw-specific-*`,ccteam-web 前端零移植。本地对照 = `references/deepseek-harness`(HEAD 2026-08-21,更新很快,以它为准不以网页文档为准)。

### PLUG-6 零配置默认态:设置卡「未配置」误导 + 面板无限转圈 + 版本错位可见性(owner Telegram 反馈 2026-08-29,规划立卡)
- **状态**:待排 · **冲突域**:`plugins/ccteam-ui/src/client/*` + `plugins/ccteam-ui/src/{bff,index,settings}.ts` + `crates/ccteam-harness/src/execution/dsh_acp/assets/ccteam-ui.tgz` · **建议入口**:UI 半规划亲自(Fable 子会话),host 半 opus maker。
- **owner 原话**:「现在这些配置用户根本不会,最好默认安装好就能用的状态」「ccteam 菜单打开后啥也没有,也无法创建项目」(截图:设置卡 REST API token / Enrollment 凭据 两栏显示「未配置」且下面是手填说明;工作台团队栏只有一个转圈)。
- **病根**:①真机是旧 daemon(0.10.3 进程,无 `/health` 身份、无 `enroll ensure`)× 新插件(0.10.4-alpha)错位 → bootstrap 拿不到凭据;插件的版本锁步横幅(D5)没能盖过团队栏的加载态,用户看到的是「空 + 转圈」而不是「引擎旧了 → 更新」;②设置卡把「由 daemon 自动提供」的凭据显示成「未配置」+ 手填指引,让零配置看起来像必填;③BFF 拉团队图失败(401/网络)时客户端停在 loading,不落到可操作状态;④零项目时「添加工作区」被加载态挡住 → 「无法创建项目」。
- **规格**:①设置卡凭据字段三态:`自动(daemon 提供)` / `手填覆盖` / `未配置(daemon 不可达)`,只有第三态才显示手填指引;②工作台加载态有上限(≤3s)与失败态:BFF 4xx/5xx/超时 → 落到首启面板(引擎面板或「添加工作区」),错误原文可展开;③版本错位(引擎旧 / 插件旧 / `/health` 缺身份字段 = 引擎早于 0.10.5)一律以横幅 + 首启面板呈现,团队栏不再转圈;④`/health` 无 `home` 字段 → 状态 `Mismatch{version}` 文案「引擎早于 0.10.5,更新引擎」而非「家目录不一致」;⑤docs `dsh-plugin*.md` §5 configure 段改成「默认零配置,何时才需要手填」。
- **DoD**:vitest 新增 ≥ 8(三态渲染 / 加载上限与失败态 / 错位横幅优先级 / 零项目 + 拉取失败仍现「添加工作区」);沙箱真机:故意用旧 daemon(0.10.3 release 二进制)+ 新插件 → 首屏是「引擎早于 0.10.5 → 更新引擎」而非转圈;正常 0.10.5 → 设置卡两栏显示「自动」。

### ACP-DISCONNECT-1 常驻 ACP 会话的对端进程被 SIGKILL 后,下一 turn 卡满 300s 才判 `stuck`(DSH2-MERGE maker 自报 2026-08-29,待排)
- **状态**:待排 · **冲突域**:`crates/ccteam-harness/src/execution/acp/`(共享 ACP 门:kimi/grok/dsh 同门)+ `crates/ccteam-harness/tests/dsh_acp_test.rs` · **建议入门**:opus maker(引擎面;PLUG-2 后串行,同 crate 不同目录可并行)。
- **现象**:daemon 持有 **live** ACP 连接时对端(真 DSH 进程)被 SIGKILL,下一条消息不立即失败,而是等满 `chat_turn_timeout`(300s)才记 `chat_turn_timeout{stuck:true}`(两次复现);会话非常驻(released)时按 sid 冷 resume 正常。
- **病根**:`execution/acp/` 的断连检测——对端 stdio/socket 关闭没有被当成「连接死亡」立即传播到 turn 驱动,只靠 turn 超时兜底。
- **做法**(根治,一层修一次):ACP 传输层把 EOF / socket 关闭 / 子进程退出统一成一个「connection lost」信号,进行中的 turn 立即以可读错误结束(`error_kind` 独立词,不混 timeout),会话标记为需要 resume(下一条消息按 sid 冷 resume,与 released 路同一条);三个 ACP vendor 同门受益。
- **DoD**:新测试:fake ACP 对端 turn 中途关连接 → 1s 内 turn 失败且 `error_kind` 非 timeout → 下一条消息同 sid 复活;`cargo test -p ccteam-harness --lib` + `--test dsh_acp_test` 绿;基线不降;真机:SIGKILL 常驻 DSH 进程后下一条消息秒级失败并自动复活。

### LIFE-2 released 会话的计数/列表走 catalog 而非每请求 `read_dir`(LIFE-1 收口时 maker 自报风险,待排)
- **状态**:待排 · **冲突域**:`crates/ccteam-im/src/{gateway.rs,session_catalog.rs}` + `crates/ccteam-web/src/routes/status.rs` · **建议入口**:opus maker(小卡)。
- **现象**:LIFE-1 起 `GET /api/v1/status` 的 `sessions_idle`(= released 计数)与 `Gateway::released_session_metas`(IM `/sessions`、MCP `session_list`、released 行解析)每次都对每个项目 `list_session_metas(dir)`(`read_dir` + 逐 `meta.json` 读)。SPA 顶栏 20s 轮询 `/status`,千会话项目 = 每 20s 千次小文件读;现网小规模无感,大 fleet 会显。
- **做法**:`SessionCatalog` 已是 O(1) sid→meta 索引 + 「首次 miss 一次懒扫」,让它对整项目维护一次性全量 warm(启动/首次列举后 `read_dir` 一次,之后靠写路径同步刷新 + inotify 或 mtime 校验),released 计数/列表从 catalog 内存视图出;`is_resumable()` 仍是唯一判据。
- **DoD**:`/status` 与 `session_list` 在 warm 后零磁盘读(`session_catalog_disk_reads()` 断言);`cargo test -p ccteam-im --lib` + `-p ccteam-web --test status_test` 绿;基线不降。

### LEDGER-1 codex 累计 token 不上账 + 新模型无价目(真机探针 2026-08-26 发现,待排)
- **状态**:待排 · **冲突域**:`crates/ccteam-harness/src/execution/codex_app_server.rs` + `crates/ccteam-cost/src/pricing.rs` + `crates/ccteam-im/src/progress_projection.rs` · **建议入口**:codex maker(小卡)。
- **现象**(STATUS-LINE 真机探针 s434/s435):codex 子会话 `context_pct` 正常(tracker 吃 `thread/tokenUsage/updated`,20717/258400),但 `tokens_total` 恒 0 —— codex 的 `TurnCompleted.usage` 为空(app-server `turn/completed` 无 usage 字段),账本只折 `chat_turn_completed.usage`,tokenUsage 通知里的 `total`/`last` 从未进 ledger;两家新模型 `gpt-5.6-sol` / `claude-sonnet-5` 无价目 → `cost_usd` null(状态行诚实回落 `k tok`,codex 连 tok 也没有)。
- **规格**:①codex adapter 在 turn 边界把 tracker 的 `last`(本 turn)token 明细填进 `TurnCompleted.usage`(一处,不动 ledger 折叠逻辑),使 `tokens_total`/成本估算与其他 vendor 同路;②价目表补 `gpt-5.6-*` 与 `claude-sonnet-5`/`claude-opus-5`/`claude-fable-5`(价格以官方页为准,owner 核),缺价仍 None 不兜底。
- **DoD**:fixture 测试 codex 一 turn 后 `tokens_total > 0`;`make check` / `make test-baseline` ≥ 1985/0 只增不减;fmt / writeback 绿。

### DSH2-CHIP 聊天内联「↗ 派给 X」(P2 可选,owner 拍板后排)
- **状态**:待排 · **冲突域**:`plugins/ccteam-ui/src/client/turnActions*` · 槽位 = `conversation.chat.assistant-actions`(list,R4 定)。

### DSH1-MODE-UI web composer 的 DSH 模式选择(跟进)
- **状态**:待排 · **冲突域**:`crates/ccteam-web/web/src/components/ChatComposer.tsx(model 菜单)` · **建议入口**:规划(控制)会话亲自(owner 先例:SPA 设计归 fable)
- **规格**:composer 的 model 菜单在 vendor=dsh 时增加第四段「模式」(标准(默认)/PTC/极简/创造 → standard/ptc/minimal/creator),draft 增 `mode`,`createSession` 已收 `mode`;非 dsh vendor 不渲染该段。
- **DoD**:vitest 三态;`make web-check` 绿;writeback 绿。

### TD-SYNC-1 tech-design 全文陈旧校对(GOV-CE-2 顺带发现)
- **状态**:待排 · **冲突域**:`docs/dev/tech-design.md` · **建议入口**:规划(控制)会话(docs 治理面)
- **背景**:GOV-CE-2 排查实锤 §0 R-code 速查漂移(R1「文件系统是状态面」/R9「crate 拓扑」不在现行 §三;R10 旧 `<team>-<slug>` 路径已随卡修正)+ 正文残留 v0.9.0 前状态(§6.x 仍写「`ccteam init` 种默认 `cto.md`」)。v0.9.10 ship gate 已顺带把三处 web 导航描述改现势(§2 前端落地注 / §6.6 统一 chat-shell 段 / 指针表 web 行),其余仍待全文轮。
- **规格**:全文一轮校对 —— R-code legend 与 body 引用对齐现行 §三(或整体改行名引用)、清 pre-v0.9.0 叙述(cto 种子/team 路径/退役命令)、协议细节改代码指针;语义争议处停手报规划。
- **DoD**:grep「种默认 cto」「<team>-<slug>」= 0 命中;R-code 引用无孤儿;最低门绿;writeback 绿。

### A2A-W5 A2A 线收尾:三场景真机 smoke + README/usage 重写
- **状态**:待排 · **冲突域**:`README.md + docs/`(smoke 零代码)· **建议入口**:规划(控制)会话(涉治理面写权)
- **背景**:v0.9.0–0.9.2 A2A 底座已落,W5 是 ship gate 前最后一步;hub 示例配方 = `team-brain` agent(grok 跨模型 review 已跑通;cct-codex/cct-grok wrapper skill 已于 2026-07-21 退役 —— MCP server instructions 原生覆盖,owner 拍板)。
- **规格**:① 三场景真机 smoke(单机委派 / 跨 vendor / 卫星跨机),结果留痕 `docs-local/versions/`;② root README + `docs/usage.md` 把 A2A 融入当前能力描述(README 英文、不写版本时间轴,规则家 = AGENTS §五.7)。
- **DoD**:三场景各一次全链路通过记录;docs-only 面走最低门(fmt + writeback);writeback 绿。

### FB-2 subagent 事件污染 live model 外显与计费捕获
- **状态**:待排 · **冲突域**:`crates/ccteam-harness(claude_stream_json)` · **建议入口**:dev 会话
- **背景**:owner 2026-07-22 实测(s106,spawn `--model fable`):主循环跑 Task subagent 期间 web 模型外显漂成 opus,subagent 结束后回落 fable;meta.json 与回落后的 status.json 均为 fable(污染瞬时)。stream-json 流里 subagent 的 assistant 事件与主循环同流,仅 `parent_tool_use_id` 可区分(`protocol.rs:261` 已解析,消费端零使用)。
- **根因**:两处消费端不过滤:① status tap `claude_stream_json/mod.rs:228` Assistant 分支把任意 assistant 事件的 `message.model` 盖进 live status(→ status.json → /sessions + web statusline/composer 外显);② `claude_stream_json/translate.rs:120-126` `turn_model` 计费捕获同源,turn 尾事件若来自 subagent 会错价整 turn。
- **规格**:model 身份只认主循环 —— 两处跳过 `parent_tool_use_id.is_some()` 的 assistant 事件;usage/token 聚合语义不动;开工时核 ACP 路(kimi/opencode)有无同类洞,有则同修。
- **DoD**:先红后绿定向测试(带 parent_tool_use_id 的 assistant 事件不改 status.model / turn_model);`make test` 基线只增;writeback 绿。

### P1-1 codex turn 粒度折叠(范围已缩:仅记账/展示面)
- **状态**:待排 · **冲突域**:`crates/ccteam-harness(codex adapter)` · **建议入口**:dev 会话
- **背景**:codex 叙述消息被当独立 turn 记账/展示(v0.9.2 遗留 P1)。**通知面已由 FB-1(e96bf56)按 turn 边界修复**;本卡余量 = turns.jsonl/展示侧的叙述折叠是否仍值得做,开工时先核现值再定。
- **规格**:折叠 codex 叙述消息进所属 turn(记账/展示);不改 `CanonicalEvent` schema 语义(schema 权威 = `harness/progress_bridge`)。
- **DoD**:新定向测试先造缺陷态红、后修绿(证有牙,留痕验证段);`make test` 基线只增;writeback 绿。

### TEST-MACOS-1 macOS 宿主两族测试环境红修复(ae24cb3 review 顺带实锤;非产品 bug)
- **状态**:待排 · **冲突域**:`crates/ccteam-core(roles tests) + crates/ccteam-harness(codex_app_server_test 基建)` · **建议入口**:dev 会话
- **背景**:两族均先于 ae24cb3、Linux CI 绿,详见 `.loop/verify/README.md` env 账「macOS 宿主两族」。① roles `list_library_skills_is_recursive_hidden_safe_and_sorted`:scanner `fs::canonicalize`(/var→/private/var)vs 测试字面 tempdir 断言,默认 shell TMPDIR 下确定性红且**在 baseline 口径内**;② codex_app_server_test 9 只 `SUN_LEN`:UDS socket 路径超长(macOS 104B 上限,长 TMPDIR 嵌套)。
- **规格**:① 测试期望 path 改按 canonicalized root 构造(生产 canonicalize 行为不动);② 测试 UDS socket 落短路径(如 `/tmp/<短随机>`,测试自清理),不动生产 socket 布局。
- **DoD**:两族在默认 shell TMPDIR(`/var/folders/…`)下全绿;`make test-baseline` 本机默认 shell 全绿;不动任何生产逻辑;writeback 绿。

### KIMI-UPSTREAM-1 kimi vendor 缺陷 watch(failed→end_turn 折叠 + 无 ctx 面)
- **状态**:待排 · **冲突域**:`crates/ccteam-harness(kimi_acp)` · **建议入口**:dev 会话
- **取活条件**:**watch 卡,平时不动** —— 仅在 kimi 升级后(或上游宣布修复)复核;无升级则本卡保持待排,不占并行位。
- **背景**:kimi 0.29.x 两处 ACP 面缺陷,已在 `410647d` 的适配器头注释中实证记录、**有意不 workaround**(owner「不要硬修」+ 不耦合 vendor 私有布局,见 state 教训行):① `turn.ended reason=failed` → `stopReason:end_turn`(仅 `provider.filtered` → refusal),error 载荷(如 10 次退避后的 `429 engine overloaded`)只进它自己的日志文件,不上线也不进 stderr → **kimi 的 turn 失败对 ccteam 全通道不可见**;② ACP 面不 push context window / token(`usage_update`/`session_info_update` 在其 schema 内但从不构造)。**②已于 v0.9.12 `80e12f6e` 按契约面解决、不再是本卡余量**:kimi 不 push 但**答** —— `status` 是它自己 `available_commands_update` 公告的命令(已公告 = 契约面,与私有日志的界线正在于此),runner 自排 turn 拉真占用,解析失败保持原值。本卡余量 = ①(仍无任何契约面信号)。诊断入口(仅人工排障用,**不得**进产品代码路径):`~/.kimi-code/sessions/wd_<slug>_<hash>/session_<vendor_uuid>/logs/kimi-code.log` + 同目录 `agents/main/wire.jsonl`。
- **规格**:每次 kimi 升级(或收到上游修复)复核 ① `stopReason` 是否透传 failed 类结局;顺带核 `usage_update` 是否终于出现(出现则 `80e12f6e` 的 probe 按其头注释「排在所有 push 通道之下」自然让位、可删)。修了则删适配器头注释对应段 + 接上共享路(`AcpStopReason` 已就位);未修则只更新版本号。**禁**:任何形式的私有 log/文件布局解析。
- **DoD**:复核结论落卡面验证段(含实测 kimi 版本号);若上游已修则新增定向测试证透传;不改红线;writeback 绿。

### DEPLOY-DRIFT-1 daemon build 漂移外显(doctor/status 比对运行中 daemon 与磁盘 binary)
- **状态**:待排 · **冲突域**:`crates/ccteam-cli(doctor/status) + crates/ccteam-core(daemon lock/version 面)` · **建议入口**:dev 会话
- **背景**:2026-07-31 实锤(state 教训「构建成功 ≠ 已部署」):tenant IM `/status` 无 👥 直接子会话的 ACL 修复(48bd3c81/e6fbef72)「修过两次仍复现」,实为运行中 daemon 仍是 Jul-29 旧映像(efce019)—— 修复 binary 建出来了但从未接管:部署软链指向 `repo/target/release`(被 `CARGO_TARGET_DIR` 重定向架空)+ daemon 未重启 + PATH 另有旧拷贝遮蔽。真实用户走 install.sh/`ccteam update` 升级后同样会踩「binary 换了、daemon 还旧」,且现状无任何面外显这个漂移(`daemon.lock` 的 `version` 字段甚至比 binary `--version` 落后一版,见背景复盘)。
- **规格**:① daemon 启动把自身 `version + build sha`(即 `--version` 同源常量)写进 lock/状态面(`daemon.lock.version` 修正为同源即可,additive 加 sha 字段);② `ccteam doctor`(可含 `status`)比对运行中 daemon 的 build 与当前 CLI binary 的 build,漂移 → 可读告警「binary 已更新,daemon 仍旧,ccteam stop && ccteam start」;③ REST 版本面外显同字段。比对认 sha 不认 mtime;单 daemon 语义与红线零碰。
- **DoD**:定向测试(漂移态告警 / 对齐态静默 / lock version 与 binary 同源);`make test` 基线只增;writeback 绿。

### P1-2 session_collect 游标去重
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_collect MCP)` · **建议入口**:dev 会话
- **背景**:collect 会重复返回已读段(v0.9.2 遗留 P1)。坐标开工时核现值。
- **规格**:collect 游标语义去重;`max_chars` 限幅与账本指针行为零碰。
- **DoD**:新定向测试先红后绿;`make test` 基线只增;writeback 绿。

### V094 npm 分发 · daemon 管理 · 自更新
- **状态**:gated(owner 2026-07-17 暂缓,v0.9.5 先行) · **冲突域**:`install.sh + crates/ccteam-cli + Makefile` · **建议入口**:版本波(doc-first)
- **背景**:PRD 已成文 `docs-local/versions/v0-9-4/prd.md`(DRAFT)。2026-07-22 起其 daemon/update 范围由 V097 PRD 承接深化,本卡剩余主体 = npm 分发面(拍板时二者收敛)。
- **规格**:占位指针卡,**不含实现授权**;拍板后由规划拆 wave 卡替换本卡。
- **DoD**:—(gated)

## 下一版候选:A2A 可观测性(蒸馏自 `docs-local/versions/v0-9-9/kimi-delegation-experience-review.md`;P0-1 已并入 v0.9.9 = V099-P0WAIT)

### A2A-OBS-1 session 内 task 一等观测(current_task / queue)
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_* 观测)` · **建议入口**:dev 会话(排期 = owner 点名下一版时)
- **背景**:复盘 P0-2(s133 任务运行 16m45s 时列表仍显健康探针 title;queue 深度不可见)。SoT 复用 delegation durable record + progress,不信 client 自报;title 只作观测标签。
- **规格**:session_list/collect 增 `current_task{turn_id,title,state,queued_at,started_at,elapsed_seconds}` + `queued_tasks`;state 集 accepted→queued→running→completed|failed|stopped。
- **DoD**:同 session 连续两 dispatch 可见 current + queue;stable title 与 task title 并显;重启后 reconcile。

### A2A-OBS-2 activity SoT 统一(TurnStarted 心跳 + last_active + 读侧并发)
- **状态**:待排 · **冲突域**:`crates/ccteam-harness + crates/ccteam-im(activity)` · **建议入口**:dev 会话(排期 = owner 点名下一版时)
- **背景**:复盘 P0-3/P0-4(同 sid idle/working 矛盾;last_active 只在 assistant turn 落地后刷;长 wait 占路径致 read-only 到 600s 点才落账)。
- **规格**:paneless TurnStarted 写 sid-tagged `chat_turn_started`(schema 权威 progress_bridge);tool/reasoning 事件刷轻量 last_event_at 心跳;live `session_list` 用 turn_started_at 即时覆盖、与持久读侧同构;last_active 在 accepted + 每个 canonical event 刷新(**TurnStarted 刷 meta.last_active 切片已于 v0.9.9 `2a2b38a` 先行落地**,消挤停误排;本卡余量 = 心跳/分类器/读侧同构);真实并发 transport 测试保 read-only 工具 15s SLA。禁 scrape / 禁因 silence kill。
- **DoD**:16min 无文本长 turn 恒 `working`;idle/working 矛盾清零;长 wait 中并发 collect/list <15s;LRU 不误排活跃 turn。

### A2A-OBS-3 ACP 首事件计时 + stop tombstone + 真机 smoke
- **状态**:待排 · **冲突域**:`crates/ccteam-harness(acp)` · **建议入口**:dev 会话(排期 = owner 点名下一版时)
- **背景**:复盘 P1-1/2/4(s130/s131 零输出无法复盘;stop 后 collect 只得 unknown)。
- **规格**:per-turn 记 `prompt_sent_at/first_event_at/first_tool_at` 等计时(记录不注入,超阈显 starting/silent 不 kill);stopped session 按 TTL 留 tombstone(倾向 24h:sid/task/title/state=stopped/时间戳/turns 指针);kimi 真机首 turn smoke 进 manual gate(不进确定性基线);候选补项(外部反馈第三轮):stale/stuck 行附静态映射 `suggested_action`(如 retry_dispatch/stop_and_respawn,纯查表零 LLM)。
- **DoD**:计时点齐可解释 s130 类事故;stop 后 collect 得 tombstone 非 unknown。

### A2A-OBS-4 完成通知 metadata-first + usage 诚实外显
- **状态**:待排 · **冲突域**:`crates/ccteam-im(通知/展示)` · **建议入口**:dev 会话(排期 = owner 点名下一版时)
- **背景**:复盘 P2-1/P2-2(kimi 最终 turn 全程叙述塞进父会话;usage 全 0 时字段消失被误读为零成本)。
- **规格**:completion notification = 固定 metadata 行(sid·title·时长·idle)+ final turn 尾部限幅(纯路由裁剪非模型总结);usage 缺失显式 `usage_source:unsupported`/`tokens_total:null`。
- **DoD**:通知形态落地;kimi session 外显 usage unavailable;不改「turn 边界一次通知」语义。

### STATE-CULL-1 ProjectState 活字段迁家退役(STATUS-SLIM-1 裁决遗留;候选无授权)
- **状态**:待排 · **冲突域**:`crates/ccteam-core(state) + crates/ccteam-im(watchdog) + crates/ccteam-cli(attach) + crates/ccteam-web(PTY/workflow)` · **建议入口**:版本波(拆卡时钉;`tmux_session` 项 gated on terminal 协议退役)
- **背景**:STATUS-SLIM-1 已把 `team`/`current_phase`/`tmux_session` 清出 MCP wire;三字段在 `ProjectState`(state.json)仍有活消费者,深退役 = 三条迁家(codex 偏差申报 B 案字面):① `team` 消费方(init refresh/migration/web API/watchdog)统一改读 catalog 后删字段;② watchdog 告警文案去 `current_phase` 依赖后删;③ project 级 terminal/PTY 路由(`core::tmux`、CLI attach/peek、PTY websocket、workflow session detail)改 per-session meta 或随 terminal 协议整体退役后删。
- **规格**:占位候选,无实现授权;拆卡时逐条钉消费方清单与测试面。
- **DoD**:—(候选)

### A2A-OBS-5 委派工效包:vendor 致命错误外显 + 派单机制补缺(v0.9.9 总控实测蒸馏)
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_* 面)` · **建议入口**:dev 会话(排期 = owner 点名下一版时;与 OBS-1..4 合并拆卡时统筹)
- **背景**:v0.9.9 规划总控实测(s134 编队 grok/codex×2/kimi):① codex s136 尾波撞「model at capacity」,完成通知形状与正常完成无异、仅凭文案可辨,恢复全靠账本中间记录 + 工作品外部化(worktree/commit);② 子会话(codex/kimi)在 session_list 全程无 tokens_total/cost,总控对整场委派零成本可见性(P2-2 之上疑 usage 捕获缺口——codex stream-json 有 usage);③ 并行编辑同仓靠 brief 纪律喊「只准在 worktree 干活」,零机制兜底(主仓 target/debug = live daemon,一走神即断桥);④ brief 传参只能同 host 绝对路径,跨机即断。
- **规格**(候选,拆卡时钉):A′. 错误通知内嵌末 1–2 条账本中间记录 + `session_collect` turn 行加 additive 错误 flag(**A 主体已于 v0.9.9 `2a2b38a` 落地**:TurnFailed/终态 Error 经 `DelegationSignal.vendor_error` 贯穿,通知冠 `[delegation completed with VENDOR ERROR]`,正常通知字节不变);B. dispatch 级 model/effort override 或保上下文 respawn(容量场景换模型不弃链);C. `session_spawn` 可选 cwd/worktree facet(local-only、项目身份不变);D. `session_dispatch` 复用 turn 附件语法(路径指针);E. 子会话 usage 捕获核查。
- **DoD**:—(占位候选卡,无实现授权)

## 历史波指针

- **v0.10.0**(DSH 第七 vendor Wave A-D `1d2cc6ea`/`3990d0f7`/`e1730938`/`03ca471c`/`f9dab6a3` + DSH web 一等公民 `97413d61`(supervisor+反代+租户物化)/`b3bd1e37`(SPA,规划亲自)/`6fb6726d`(用户面文档)/`e25e6e2c`(clippy)/`09245d32`(start_for 自死锁修复)/`c999509a`(租户 patch insert+嵌套 config 修复 + randomUUID polyfill);明细 → `docs-local/versions/v0-10-0/`(原 `v0-9-15/` 改名)+ `.loop/history.md` 一行)· **v0.9.12**(累积周期,全程 owner 直驱**无卡** —— 本节只作坐标:spawn 调参轴 `4d223cf5`/`02c6d1b5`/`a0b714f9`/`13d9ace7`/`daef69b0` · 上下文口径 `b6634b26`/`0dcce1da`/`80e12f6e` · 团队拓扑强度列 `18a79f04`/`00b622ab` · MCP 传输统一 HTTP `1ce65b86`/`379cd2b2` · install 落点阶梯 `08aa865e`/`53074ff8`/`ffc86515` · ACP 结局契约 `410647d5` · 租户面五修 `d66cb75a`/`5a62ae0f`/`53a06a09`/`89cc7a40`/`48bd3c81`+`e6fbef72`;一行史 → `.loop/history.md`)· **v0.9.11**(团队页驾驶舱重设计:TEAM-1 `33545de5` 拓扑独占+真链接+chips+ticker / TEAM-2 `9609eb37` routing REST+宪章编辑器+名册 / TEAM-3 `670e335f` playbooks 6 编队 / TEAM-4 `e6704daf` live model join / wave 修复 `b20e1e96` sessions_api 封口 / TEAM-5 `4c45ed01` host 反注册 REST+CLI / TEAM-6 `61692685` 名册按主机分组+在线离线+移除 / TEAM-7 `8ec9cf2e` 名册卡点击过滤拓扑 / TEAM-8 `ee32b6cd` 离线时长+stale 建议 / TEAM-9 `3621e871` HostsView 收敛动作面 / TEAM-10 `36c5793a` npm 可更新提示迁名册;明细 → `docs-local/versions/v0-9-11/`)· **v0.9.10**(MCP 工具面治理 + doctor 重排与自动注册 + web IA 改版 + IM 下一步提示 + 活跃消息 vendor 注入 + web ACL 收敛;完成卡明细 → `docs-local/versions/v0-9-10/`)· v0.9.9(全局 skill 库 + wait 240 诚实 pending + 烂测清理;明细 → `docs-local/versions/v0-9-9/README.md`)· v0.9.7(daemon Codex pid-detach 重构 + `ccteam update`,PR #165 `825ae7d`)· v0.9.2 及此前 → `.loop/history.md`(每版一行)+ `git log` + `docs-local/versions/`(gitignored 详档)
