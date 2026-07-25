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

### V099-SHIP 文档 + version bump + 治理回填(规划)
- **状态**:完成(6b8211f) · **冲突域**:`docs/ + AGENTS.md + .loop/ + Cargo.toml` · **建议入口**:规划(控制)会话
- **验证**:usage/usage-cn/README/tech-design(含陈旧 cto 表述清理)+ AGENTS §〇/§一/§四 + workspace 0.9.9 + lock 刷新 + `.loop` 蒸馏回填全部入库;writeback 绿;**dev→main PR #169 已开**(CI 三 job 全绿),tag/部署 HELD 等 owner。
- **规格**:usage(+cn)/tech-design/README 把 skill 库融入当前能力;AGENTS §一版本行 + §四 Skills 行;workspace 0.9.9;state/history/backlog 蒸馏;P2-1 CI job 顺带(SSH push)。

### MCP-DX-1 外部 agent 反馈:MCP 工具面 DX(发现性 + 可操作错误 + 完成遥测;净减法)
- **状态**:完成(cf49539) · **冲突域**:`crates/ccteam-im/src/mcp` · **建议入口**:规划(控制)会话(owner 直驱 2026-07-24)
- **验证**:7 个新定向测试有牙实锤(三处缺陷态突变咬红 4 测试→复原后绿);`ccteam-im` mcp 模块 113 绿;`make check` clippy 0 warnings;`make test-baseline` 本分支 +7、无新增红(本机 3 只口径内红 = 宿主态泄漏,`git stash` 对照 origin/dev **同机同红**归因非本改动,见 HERM-1 卡;干净环境仲裁 = PR CI);writeback 绿。描述净减法量化:8 工具描述总量 6210→5418 字符(-792;spawn -607、dispatch -439,与 schema property 文档去重;status +254 = 39 字符占位升级为发现面)。全量 `make test` 在本机撞已登记 `hook_*` env-flake 挂死(README 在案),不计入判据。
- **背景**:三份外部 agent 调用复盘(codex/workbuddy/qoder,owner 2026-07-24 转交):① "grok" 关键词搜不到 spawn(vendor 埋描述中段);② project 解析失败是死胡同(cwd≠catalog slug 只能瞎猜,`missing project`/`not found` 无恢复指引);③ 同步等待完成不带成本/耗时。owner 追加钢线:MCP 面向 agent,改进 ≠ 更多更长。
- **规格**:A. 发现性(净减法)—— spawn 描述 vendor 五选一提至第一句 + 一句用途提示,与 property 文档去重;dispatch 同步瘦身;`status` 描述升级为发现面(vendor panel/catalog/routing,**替代**外部建议的新 capabilities 工具);instructions 补 Kimi。B. 可操作错误 —— admin missing/unknown project 附注册清单(cap 20)+ did-you-mean(Levenshtein/containment,离谱输入不瞎猜);tenant 错误附自己可见清单(纯 identity 派生,foreign/unknown 字节一致不泄露,原测试收紧为显式一致性断言)。C. 完成遥测 —— inline-wait completed 增 `elapsed_seconds`(submit→完成,0.1s 分辨率)+ `tokens_total`(会话累计账,同 list/collect 语义);additive 字段,8-tool wire schema 形状零改动。**明确不做**(记录在案):新工具 ask/vendors/project_list/per-vendor alias(违背 v0.9-T1 cull;发现性走描述+status);过程叙述与最终答案分离(ACP 整 turn 单 TurnBuffer 结构性,归 A2A-OBS-4 族);wait 心跳/进度通知(同族);response_format/json_schema(vendor-specific,prompt 层可达);qoder「vendor 非 schema enum」系过时(已是 enum);workbuddy「ACP spawn+task+wait Connection closed」判客户端超时(服务端预算 60s+wait 正确,idempotency_key 即正解),无 repro 不动。
- **DoD**:达成(见验证段)。

### A2A-W5 A2A 线收尾:三场景真机 smoke + README/usage 重写
- **状态**:待排 · **冲突域**:`README.md + docs/`(smoke 零代码)· **建议入口**:规划(控制)会话(涉治理面写权)
- **背景**:v0.9.0–0.9.2 A2A 底座已落,W5 是 ship gate 前最后一步;hub 示例配方 = `team-brain` agent(grok 跨模型 review 已跑通;cct-codex/cct-grok wrapper skill 已于 2026-07-21 退役 —— MCP server instructions 原生覆盖,owner 拍板)。
- **规格**:① 三场景真机 smoke(单机委派 / 跨 vendor / 卫星跨机),结果留痕 `docs-local/versions/`;② root README + `docs/usage.md` 把 A2A 融入当前能力描述(README 英文、不写版本时间轴,§三红线)。
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

### HERM-1 基线口径内 3 测试宿主态泄漏(live 机红 / 干净环境绿)
- **状态**:待排 · **冲突域**:`crates/ccteam-cli(web_chat_bridge) + crates/ccteam-core(roles) + crates/ccteam-harness(transcript_tail)` · **建议入口**:dev 会话
- **背景**:MCP-DX-1 收口实测(2026-07-25,live-daemon 宿主):`make test-baseline` 口径内 3 红,`git stash` 对照 origin/dev **同机同红**、CI 干净环境全绿(v0.9.9 tip)——违反「基线口径内测试必须密封」纪律(verify/README):① `web_chat_bridge::web_chat_ws_routes_through_gateway_and_survives_restart`(live daemon 端口/socket 争用嫌疑);② `roles::list_library_skills_is_recursive_hidden_safe_and_sorted`(疑读真实 `~/.ccteam/skills` —— v0.9.9 全局库新面,隔离助手须同时 pin HOME+CCTEAM_HOME,AGENTS §六);③ `execution::transcript_tail::discover_skips_subagent_jsonls_even_when_newest`(单测绿、全套并发红 = 套内相互作用/真实 `~/.claude/projects` 泄漏嫌疑)。
- **规格**:逐只归因 + 注入缝密封(参照 0ec136d per-Gateway 快照先例;禁 env 突变);先红后绿留痕。
- **DoD**:live-daemon 宿主上 `make test-baseline` 全绿;CI 同绿;writeback 绿。

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

### P2-1 CI 增确定性测试 job
- **状态**:完成(6b8211f) · **冲突域**:`.github/workflows/` · **建议入口**:规划(控制)会话(治理面;改 workflow 须 SSH push,§六)
- **验证**:PR #169 CI 三 job 全绿(fmt 18s / clippy 1m54s / test 2m51s,run 30038720051)。**门有牙实锤 = 首跑即红**:干净 runner 咬出 `session_tool_tests` 15 个隐性 PATH 依赖(开发机 vendor 常驻致本地恒绿假象)→ hermetic 注入缝 `0ec136d`(per-Gateway 可用性快照;无 env 突变、不出 lib 口径、生产探测不变;红后绿双 PATH 证)→ 复跑全绿。口径 `--lib --bins --locked`(对齐 41c6569 修正;卡面原文 `--lib` 系修正前拟定)。
- **背景**:V095 复核发现 `check.yml` 只跑 fmt + clippy,**测试完全不在 CI** —— 基线只增当前全靠会话自律 + 复核(P1-3 的测试陈化即因此漏网)。确定性口径(`--lib`)本就为免 env-flake 设计,适合上 CI。
- **规格**:加 job `cargo test --workspace --exclude ccteam-web --lib --locked`;**不**上 web/e2e(env 依赖);过门后同步 `.loop/verify/README.md` 门禁地图。
- **DoD**:CI 三 job 绿;writeback 绿。

## 下一版候选:A2A 可观测性(蒸馏自 `docs-local/versions/v0-9-9/kimi-delegation-experience-review.md`;P0-1 已并入 v0.9.9 = V099-P0WAIT)

### A2A-OBS-1 session 内 task 一等观测(current_task / queue)
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_* 观测)` · **建议入口**:dev 会话(排期 = v0.9.9 后)
- **背景**:复盘 P0-2(s133 任务运行 16m45s 时列表仍显健康探针 title;queue 深度不可见)。SoT 复用 delegation durable record + progress,不信 client 自报;title 只作观测标签。
- **规格**:session_list/collect 增 `current_task{turn_id,title,state,queued_at,started_at,elapsed_seconds}` + `queued_tasks`;state 集 accepted→queued→running→completed|failed|stopped。
- **DoD**:同 session 连续两 dispatch 可见 current + queue;stable title 与 task title 并显;重启后 reconcile。

### A2A-OBS-2 activity SoT 统一(TurnStarted 心跳 + last_active + 读侧并发)
- **状态**:待排 · **冲突域**:`crates/ccteam-harness + crates/ccteam-im(activity)` · **建议入口**:dev 会话(排期 = v0.9.9 后)
- **背景**:复盘 P0-3/P0-4(同 sid idle/working 矛盾;last_active 只在 assistant turn 落地后刷;长 wait 占路径致 read-only 到 600s 点才落账)。
- **规格**:paneless TurnStarted 写 sid-tagged `chat_turn_started`(schema 权威 progress_bridge);tool/reasoning 事件刷轻量 last_event_at 心跳;live `session_list` 用 turn_started_at 即时覆盖、与持久读侧同构;last_active 在 accepted + 每个 canonical event 刷新(**TurnStarted 刷 meta.last_active 切片已于 v0.9.9 `2a2b38a` 先行落地**,消挤停误排;本卡余量 = 心跳/分类器/读侧同构);真实并发 transport 测试保 read-only 工具 15s SLA。禁 scrape / 禁因 silence kill。
- **DoD**:16min 无文本长 turn 恒 `working`;idle/working 矛盾清零;长 wait 中并发 collect/list <15s;LRU 不误排活跃 turn。

### A2A-OBS-3 ACP 首事件计时 + stop tombstone + 真机 smoke
- **状态**:待排 · **冲突域**:`crates/ccteam-harness(acp)` · **建议入口**:dev 会话(排期 = v0.9.9 后)
- **背景**:复盘 P1-1/2/4(s130/s131 零输出无法复盘;stop 后 collect 只得 unknown)。
- **规格**:per-turn 记 `prompt_sent_at/first_event_at/first_tool_at` 等计时(记录不注入,超阈显 starting/silent 不 kill);stopped session 按 TTL 留 tombstone(倾向 24h:sid/task/title/state=stopped/时间戳/turns 指针);kimi 真机首 turn smoke 进 manual gate(不进确定性基线)。
- **DoD**:计时点齐可解释 s130 类事故;stop 后 collect 得 tombstone 非 unknown。

### A2A-OBS-4 完成通知 metadata-first + usage 诚实外显
- **状态**:待排 · **冲突域**:`crates/ccteam-im(通知/展示)` · **建议入口**:dev 会话(排期 = v0.9.9 后)
- **背景**:复盘 P2-1/P2-2(kimi 最终 turn 全程叙述塞进父会话;usage 全 0 时字段消失被误读为零成本)。
- **规格**:completion notification = 固定 metadata 行(sid·title·时长·idle)+ final turn 尾部限幅(纯路由裁剪非模型总结);usage 缺失显式 `usage_source:unsupported`/`tokens_total:null`。
- **DoD**:通知形态落地;kimi session 外显 usage unavailable;不改「turn 边界一次通知」语义。

### A2A-OBS-5 委派工效包:vendor 致命错误外显 + 派单机制补缺(v0.9.9 总控实测蒸馏)
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_* 面)` · **建议入口**:dev 会话(排期 = v0.9.9 后;与 OBS-1..4 合并拆卡时统筹)
- **背景**:v0.9.9 规划总控实测(s134 编队 grok/codex×2/kimi):① codex s136 尾波撞「model at capacity」,完成通知形状与正常完成无异、仅凭文案可辨,恢复全靠账本中间记录 + 工作品外部化(worktree/commit);② 子会话(codex/kimi)在 session_list 全程无 tokens_total/cost,总控对整场委派零成本可见性(P2-2 之上疑 usage 捕获缺口——codex stream-json 有 usage);③ 并行编辑同仓靠 brief 纪律喊「只准在 worktree 干活」,零机制兜底(主仓 target/debug = live daemon,一走神即断桥);④ brief 传参只能同 host 绝对路径,跨机即断。
- **规格**(候选,拆卡时钉):A′. 错误通知内嵌末 1–2 条账本中间记录 + `session_collect` turn 行加 additive 错误 flag(**A 主体已于 v0.9.9 `2a2b38a` 落地**:TurnFailed/终态 Error 经 `DelegationSignal.vendor_error` 贯穿,通知冠 `[delegation completed with VENDOR ERROR]`,正常通知字节不变);B. dispatch 级 model/effort override 或保上下文 respawn(容量场景换模型不弃链);C. `session_spawn` 可选 cwd/worktree facet(local-only、项目身份不变);D. `session_dispatch` 复用 turn 附件语法(路径指针);E. 子会话 usage 捕获核查。
- **DoD**:—(占位候选卡,无实现授权)

## 历史波指针

- v0.9.9(全局 skill 库 + wait 240 诚实 pending + 烂测清理,dev→main PR 待 owner 合并;蒸馏出的完成卡明细 → `docs-local/versions/v0-9-9/README.md`)· v0.9.7(daemon Codex pid-detach 重构 + `ccteam update`,PR #165 `825ae7d`)· v0.9.2 及此前 → `.loop/history.md`(每版一行)+ `git log` + `docs-local/versions/`(gitignored 详档)
