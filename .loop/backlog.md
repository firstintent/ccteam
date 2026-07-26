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

### MCP-DX-3 外部反馈第三轮:beacon 别名改名 + project 默认梯(owner 直驱 2026-07-26「必须优化」两项)
- **状态**:待排 · **冲突域**:`crates/ccteam-im/src/mcp + crates/ccteam-cli + crates/ccteam-core(config/projects) + crates/ccteam-web(SPA 文案/tests) + docs` · **建议入口**:codex 委派(规划发卡 + review;**域串行**:与 HERM-1A 同前缀 `crates/ccteam-im`+`ccteam-cli`,等其收口后接续派 s153)
- **背景**:第三轮外部复盘(robchat 宿主,旧二进制)。**已是现势不再动**:wait 240 诚实上限(v0.9.9)/ collect `total_chars`+`truncated`(响应实测有,定向测试在案)/ 完成遥测 tokens·cost(MCP-DX-1)/ 单项目默认+可操作错误(MCP-DX-1/2)。**拒绝项**(记录在案):grok_search/quick_search 高层工具(钢线「改进≠加法」,A 改名即发现面答案)/ project schema enum(宿主缓存 tools/list → 新项目被客户端 schema 拒)/ spawn `reuse:true`(破「永铸新 sid」契约+上下文污染;dispatch-to-existing 已是复用正道,session_list 描述已导)/ 智能截断(账本指针+`since` 游标可取全文)/ status 并 verbose 参数(纯别名为裸名宿主而生,响应等同零认知成本)/ wait 心跳保活(家 = A2A-OBS-4 族已记)。
- **规格**:A. 别名改名(owner 钦点字面):`claude_codex_grok_kimi_opencode_status` → **`grok_claude_codex_kimi`**(仍 = status 纯别名、响应逐字节等同;grok 打头 = 搜索发现面,去 `_status` 尾缀,opencode 出列;旧名即删无 shim,pre-v1)。三路同判(protocol 定义 / dispatch is_status_call / cli forward_status)+ doctor --verify-mcp 期望集同步;等价测试保留;MCP-BEACON-1 派生锁测试(全员恰一次+opencode 殿后)按 owner 新字面替换为**字面锁**(64 上限断言保留);文档/SPA 名字清扫(README/usage±cn/orchestration±cn/tech-design census/WorkflowView 文案;AGENTS §四 = 规划收口时改)。B. project 默认梯(owner 拍板「服务端默认项目」+ 规划设计定形):Admin 缺省解析 = explicit > `_caller_slug`(cwd,既有)> sole(既有)> **configured**(`~/.ccteam/config.yaml` 新可选键 `default_project: <slug>`,use-time 校验存在,无效跳过不报错)> **scratch 自动供给**(`~/.ccteam/default_project` 就地 init + catalog 注册;按**路径**反查已注册条目,slug 钉 `default` 撞名累加;lazy 首用创建、幂等;真项目 = scaffold/账本/ACL 全常规);tenant 语义零变(仍须点名,sole-visible 默认在案)。spawn 响应加 additive **`project_source`**(`explicit|principal|cwd|sole|configured|scratch`,诚实观测);**scratch 命中时响应另附一句静态 `note`**(owner 追加 2026-07-26 并钉死语序:**先干活后提示** —— spawn 照常成功执行进 default,`note` 仅事后随成功响应告知「当前在 default 项目,如需特定项目工作区请显式传 `project`,`status` 列 slug」;绝不先问再干、不因缺省阻塞或报错;纯静态字符串零 LLM,仅 scratch 级出现);missing-project 死胡同对 admin 消失,错误路径仅剩 tenant 未点名。
- **DoD**:定向测试先红后绿(A 字面锁+等价;B 梯级:configured 命中 / 无效 configured 跳过 / scratch 首用创建+二次幂等复用 / `project_source` 各值);doctor --verify-mcp 8 工具 0 STUB(新名);`make check` clippy 0;`make test-baseline` 只增;`make web-check` 绿;fmt 干净;writeback 绿;两 commit 收口(实现→写回)。

### STATUS-SLIM-1 status 返回省 token:退役字段全链清除(owner 直驱 2026-07-26「不能变 token 刺客;功能优先、省 token 其次;从多层通用性对称性修」)
- **状态**:待排 · **冲突域**:`crates/ccteam-core(state/queries) + crates/ccteam-im/src/mcp + crates/ccteam-cli(status render) + crates/ccteam-web(如有引用) + docs` · **建议入口**:codex 委派(规划发卡 + review;域串行:排 MCP-DX-3 后同 s153)
- **背景**:MCP `status` JSON 半数字段是考古死载荷,逐调用烧调用方上下文(9 项目 × 9 字段实测):`protocol.rs tool_ls_matching` 逐项目发 `team`(退役概念,core 零 team 红线)/`current_phase`+`phase_state`(状态机 V0.4.0 已删,注释自证)/`tmux_session`(terminal 时代,stream-json 会话无)/`age_seconds`(巨数无用)/`cost_used_usd`+`cost_active_usd`(orchestrator 时代口径);外层 `"orchestrator"` 块 = `active_count` 硬编码 0 + `MAX_CONCURRENT_PROJECTS`(退役概念),连块名都是考古。死字段的**家 = core `ProjectState`**(state.json schema)——多层对称修 = core 退役,消费面全同步。**原则钉死:功能优先、省 token 其次**——vendor 面板/catalog/routing/recipes(发现面价值主体)零删。
- **规格**:A. core:`ProjectState` 退役 `team`/`current_phase`/`phase_state`/`tmux_session` 字段 + `PhaseState` 枚举 + `MAX_CONCURRENT_PROJECTS`(grep 全 caller,仅状态渲染引用则删;有活引用停手偏差申报);serde 兼容 = 旧 state.json 携带死键照常解析忽略(pre-v1 不写迁移,init 不再写入);`age_seconds` 出状态渲染(struct 如有他用保留)。B. MCP status JSON 瘦形:projects[] → `{slug, cost_24h_usd}`;`"orchestrator"` 块整体删除,换顶层 `"daemon": {status, message}`(socket 并入 message 不重复发);tenant 版(`tool_ls_for_user`)同形;vendor panel/catalog/routing/recipes 文本面零碰;beacon 别名等价测试自然覆盖新形。C. 对称清扫:CLI `ccteam status` 人读版 + REST/web 视图 + SPA 对被退役字段的引用(rg 逐字段)同步删;IM `/status` 渲染如引用同步。D. docs:tech-design 涉及行 + usage 如有字段枚举同步。
- **DoD**:MCP status JSON 键集定向测试(恰为瘦形键,死键反断言)+ 旧 state.json 带死键解析测试;字段 grep 全仓 0 残留(注释/history 除外);`make check` clippy 0;`make test-baseline` 红不增、数目净变逐只对账;`make web-check` 绿(如触 SPA);fmt;writeback 绿;两 commit 收口。

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

### HERM-1 web_chat_bridge restart-restore 竞态(**PR #170 CI 红 = merge blocker**)+ 宿主态泄漏两只密封
- **状态**:进行中(codex 委派·2026-07-26) · **冲突域**:`crates/ccteam-cli(web_chat_bridge) + crates/ccteam-im(gateway restore) + crates/ccteam-core(roles) + crates/ccteam-harness(transcript_tail)` · **建议入口**:codex 委派(规划发卡 + review)
- **背景(2026-07-26 规划复诊,原「宿主态泄漏×3」归因对 ① 作废)**:① `web_chat_bridge::web_chat_ws_routes_through_gateway_and_survives_restart` **:722 `assert_eq!(sessions.len(), 2)` left=1** —— daemon 重启后 `/sessions` 只见 1/2 会话(restore 自 v0.8.21 = 逐会话 resume-aware `start_thread` 重生)。本机 5/5 稳红(0.16s;**父提交 2963424b 同红** = 与 e9c13043 看门狗提交无关);CI 自 2026-07-25 15:30 边界起**多红偶绿**(run 30206871136 同断言;eea47f2b 含同代码一次绿 = 概率性)。同断言两环境复现 = **restart-restore 竞态真回归/真产品行为**(引入点 ≤2963424b 未定),旧「live 机红/CI 绿=环境态」判据失效;**PR #170 merge 等本卡 A 绿**。② `roles::list_library_skills_is_recursive_hidden_safe_and_sorted` + ③ `transcript_tail::discover_skips_subagent_jsonls_even_when_newest` = 宿主态泄漏族(本周期本机转绿,漂移本性),密封目标不变(② 疑读真实 `~/.ccteam/skills`,隔离须同时 pin HOME+CCTEAM_HOME;③ 套内并发相互作用/真实 `~/.claude/projects` 嫌疑)。
- **规格**:A(优先,blocker). 根因归因 restart→restore→`/sessions` 竞态并修**确定性**:先查第二会话丢在哪层(restore 未跑完?重生失败被吞?列表渲染过滤?)——若 restore 异步与首个 `/sessions` 竞速,倾向产品侧诚实序(restore 完成信号/屏障,或 bridge 既有 ready 信号接线;daemon 不因此阻塞服务);**禁 sleep 修补**(测试侧带 deadline 的轮询可接受,产品侧优先);不弱化断言。修后本机(现稳红 = 天然 red→green 有牙)与 CI 双绿。B. ②③ 注入缝密封(参照 0ec136d per-Gateway 快照;禁 env 突变)。
- **DoD**:A:该测试本机连跑 ≥20 次全绿 + `make test-baseline` 全绿(1667/0)+ push 后 PR #170 CI 三 job 绿;B:先红后绿留痕(可复现缺陷态)或如实报告不可复现;clippy 0;fmt 干净;writeback 绿;两 commit 收口(实现→写回)。
- **验证(A)**:实现 `3413364`。根因实锤:daemon 把 `resume_restored_sessions_shared` 放入独立 scheduler task 后立即启动 channel listener/inbound consumer;restore 逐 sid spawn→apply,首个 restart `/sessions` 恰在 s1/claude 已 apply、s2/codex 尚未 apply 时读 live map(补诊断后本机稳定显示仅 s1),非 spawn 错误/列表过滤。修复 = scheduler 在完整 restore 后发 watch completion;仅 restore 期间到达的 `/sessions` 独立任务等待该信号后再读 gateway,其余 inbound 与 web/daemon 启动不阻塞,零 sleep。原测试先红(left=1),最终形态保 exactly 2 并加 same sids=`[s1,s2]`,连续 **20/20** 绿;`make test-baseline` **1667/0**(②③ 本轮均绿);fmt 绿;`make check` clippy 0;`writeback.sh` 绿。Part B 零碰,本卡状态保持进行中。

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

### A2A-OBS-5 委派工效包:vendor 致命错误外显 + 派单机制补缺(v0.9.9 总控实测蒸馏)
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_* 面)` · **建议入口**:dev 会话(排期 = owner 点名下一版时;与 OBS-1..4 合并拆卡时统筹)
- **背景**:v0.9.9 规划总控实测(s134 编队 grok/codex×2/kimi):① codex s136 尾波撞「model at capacity」,完成通知形状与正常完成无异、仅凭文案可辨,恢复全靠账本中间记录 + 工作品外部化(worktree/commit);② 子会话(codex/kimi)在 session_list 全程无 tokens_total/cost,总控对整场委派零成本可见性(P2-2 之上疑 usage 捕获缺口——codex stream-json 有 usage);③ 并行编辑同仓靠 brief 纪律喊「只准在 worktree 干活」,零机制兜底(主仓 target/debug = live daemon,一走神即断桥);④ brief 传参只能同 host 绝对路径,跨机即断。
- **规格**(候选,拆卡时钉):A′. 错误通知内嵌末 1–2 条账本中间记录 + `session_collect` turn 行加 additive 错误 flag(**A 主体已于 v0.9.9 `2a2b38a` 落地**:TurnFailed/终态 Error 经 `DelegationSignal.vendor_error` 贯穿,通知冠 `[delegation completed with VENDOR ERROR]`,正常通知字节不变);B. dispatch 级 model/effort override 或保上下文 respawn(容量场景换模型不弃链);C. `session_spawn` 可选 cwd/worktree facet(local-only、项目身份不变);D. `session_dispatch` 复用 turn 附件语法(路径指针);E. 子会话 usage 捕获核查。
- **DoD**:—(占位候选卡,无实现授权)

## 历史波指针

- **v0.9.10**(MCP 工具面治理 + doctor 重排与自动注册 + web IA 改版 + IM 下一步提示;PR #170 ready 待 owner 合并;完成卡明细 → `docs-local/versions/v0-9-10/`)· v0.9.9(全局 skill 库 + wait 240 诚实 pending + 烂测清理;明细 → `docs-local/versions/v0-9-9/README.md`)· v0.9.7(daemon Codex pid-detach 重构 + `ccteam update`,PR #165 `825ae7d`)· v0.9.2 及此前 → `.loop/history.md`(每版一行)+ `git log` + `docs-local/versions/`(gitignored 详档)
