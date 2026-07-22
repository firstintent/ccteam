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

### V096-R1 routing notes 归位项目根 + 全局默认文件
- **状态**:完成(9c5f895) · **冲突域**:`crates/ccteam-core + crates/ccteam-im(mcp/vendor_panel) + Makefile + docs/` · **入口**:owner 直驱
- **背景**:v0.9.6 初版把项目级路由放在 `~/.ccteam/routing/projects/<slug>.md`,与 project 一等实体的归属不一致;全局 `routing.md` 也只读不生成。
- **规格**:项目覆盖改为 `<project>/.ccteam/routing.md`,项目级 > `~/.ccteam/routing.md` 全局 fallback,二者不合并;统一 home ensure 仅在全局文件缺失时生成中立默认,绝不覆盖;不为每个项目自动生成覆盖,不留旧路径兼容分支。
- **DoD**:先红后绿定向测试覆盖路径优先级/旧路径退役/全局生成不覆盖;Rust 门禁 + docs 同步 + writeback 绿;提交并推送远程 `dev`。
- **验证**:先红(`ccteam-core` 缺统一 path/default API 编译失败)后绿;routing 定向 2+4/0;`make check` 绿(clippy 0 warnings);`cargo test --workspace --exclude ccteam-web --lib --locked` **1472/0**(1469+3);fmt/diff-check 干净。
- **偏差**:`make test` 非零仅见已有无关失败:remove_test t03/t17(已立 P1-3)、resume/inbound env-flake 族,以及 CLI web-chat timeout;后者在本改动前 `3e6bca1` 临时 worktree 同用例同样失败,证非本卡回归。本卡不越域修旧测试。

### A2A-W5 A2A 线收尾:三场景真机 smoke + README/usage 重写
- **状态**:待排 · **冲突域**:`README.md + docs/`(smoke 零代码)· **建议入口**:规划(控制)会话(涉治理面写权)
- **背景**:v0.9.0–0.9.2 A2A 底座已落,W5 是 ship gate 前最后一步;hub 示例配方 = `team-brain` agent(grok 跨模型 review 已跑通;cct-codex/cct-grok wrapper skill 已于 2026-07-21 退役 —— MCP server instructions 原生覆盖,owner 拍板)。
- **规格**:① 三场景真机 smoke(单机委派 / 跨 vendor / 卫星跨机),结果留痕 `docs-local/versions/`;② root README + `docs/usage.md` 把 A2A 融入当前能力描述(README 英文、不写版本时间轴,§三红线)。
- **DoD**:三场景各一次全链路通过记录;docs-only 面走最低门(fmt + writeback);writeback 绿。

### FB-1 MCP 委派面反馈修复(通知=turn 边界 + 超时 + list 过滤 + token 账)
- **状态**:完成(e96bf56) · **冲突域**:`crates/ccteam-im + crates/ccteam-harness(delegation/meta)` · **入口**:规划会话(owner 2026-07-18 直驱,Fable 5 亲自实现)
- **背景**:owner 提交 excore 实测反馈(s69 codex 委派):P0×2(叙述逐条通知洪泛 / idle 停摆无信号,共同根因=通知单位是 assistant 消息而非任务)+ P1(collect/list 无超时整 turn 悬挂)+ P2×2(非 claude vendor 零落账 / session_list 无过滤全量灌上下文)。
- **落地**:通知时机改 vendor turn 边界(pump 聚合中途叙述,`TurnCompleted/TurnFailed/Error` 才发,文案明示 child 已 idle+折叠计数);`notify: final(默认)/all/off`(bool 兼容);wait 等真边界不被叙述提前结束;session_* 全带服务端超时;ACP 也 mirror `chat_turn_completed` + meta `tokens_total`;list 加 `project/activity/limit` + 行瘦身 + task 首行派生 title;重启 reconcile 卷总。
- **验证**:lib 基线 1435→1447/0;clippy 0;web 与 origin/main 同基线(3 `ws_*` env-flake 族不变);+12 定向测试(边界折叠 e2e / all·off 模式 / dedup 重放 / reconcile 卷总 / wait-vs-叙述 / list 过滤 / 派生 title / NotifyMode wire / 旧 bool watch 兼容);writeback 绿。未部署(tag+部署 HELD 不变)。

### EXT-MCP-1 外部 Agent MCP 接入 Phase 1(WebUser project ACL)
- **状态**:完成(e25544d) · **冲突域**:`crates/ccteam-web(routes/mcp,auth) + crates/ccteam-im(mcp) + crates/ccteam-core(identity)` · **入口**:codex dev 会话(s118 派工,规划 review 把关)
- **背景**:研究稿 `docs-local/research/external-agent-mcp-symmetric-architecture.md`(2026-07-23);owner 同日拍板按推荐默认(D1–D10)实现 **Phase 1**。缺口已逐条核实:①`/mcp` 拒 tenant web token(`routes/mcp.rs::require_mcp_auth` 只收 admin+session bearer)②`McpCaller` 只有 `Ambient|Admin` ③ MCP 面无 project ACL choke point ④ spawn owner 硬编码 `"web-api"`(`dispatch.rs:1238`)⑤ `chat_send_file`/`screenshot` dispatch 不带 caller。
- **规格**:仅 Phase 1(主 daemon 单 Authority):`/mcp` 经 `resolve_identity` 族收 tenant token → 新 `McpCaller::User{user_id}`(`Copy`→`Clone`);身份策略纯函数(owner_tag/can_see_owner)下沉 `ccteam-core`,web `Identity` 与 im 共用;User 调 session_*:spawn 必带显式 `project` + ACL(仅自有项目),owner/reply_to 归 `user:<id>`(与 REST create_session 同路),root spawn(无 parent edge);dispatch/collect/stop 先 sid→project 再 ACL,unknown 与 forbidden 错误不可区分(防枚举);list/status 只聚合该用户可见项目;screenshot 先 slug ACL;chat_send_file 投递本人 linked IM、无绑定可读错误、零收件人参数。**不改 8-tool wire schema;不加 `host` 参数;tenant 绝不映射 Admin;Ambient/Admin 现行为零回归;Phase 2–4 不做**。
- **DoD**:定向测试先红后绿(tenant bearer 认证/ACL 允許+拒绝/防枚举统一错误/owner 归属/防 `_caller_*` 伪造);`make test-baseline` ≥1604 + `ccteam-web` 全量不退;clippy 0 warnings;fmt 干净;writeback 绿;落 `dev` 汇入 PR #166。
- **验证**:规划独立复跑门禁(不信转述):fmt 干净;clippy 0 warnings;确定性基线 **1615/0**(1604+11);web 全量 0 失败(`pty_ws_test` 3 `ws_*` = 已登记 env-flake 族,main=dev 同红且相关文件零 diff);逐块 diff review 通过(tenant 绝不映射 Admin/Ambient、`_caller_*` 前缀全量剥离 fail-closed、unknown/forbidden 错误逐字节相同、spawn owner 归 `user:<id>` 与 REST 同种子、策略单源 `ccteam-core::identity`)。ff 合入 dev(e25544d)。
- **偏差**:无。遗留观察(非阻塞):`_caller_visible_projects` 服务端注入搭 args 便车,Ambient 客户端可自带但只能收窄列表、无扩权面;后续整洁度改进可立小卡。

### WEB-SSE-1 web 聊天断线后转录丢失(重连回填 + 复活)
- **状态**:完成(e3dfe5c) · **冲突域**:`crates/ccteam-web/web/src(hooks/useSessionEvents,pages/SessionView)` · **入口**:codex dev 会话 s119(规划派工 + 把关)
- **背景**:owner 实测(s117,2026-07-23):web 页挂起期间 SSE 断线,回填仅剩有界 replay ring,超窗的 answer 帧(s117-2/3 两条收尾汇报)永久不渲染;`MAX_RETRIES=7`(~90s)后 SSE 永久放弃,页面死透只能手动刷新。历史 seed 只在挂载时跑一次(`SessionView.tsx:91`)。
- **规格**:① SSE 断线→重连成功的 transition 上重拉 `getHistory` 作权威回填(替换 rows + 正确重置 fold 游标,不重复不丢行);② `visibilitychange→visible` / `online` 事件重置重试计数并立即重连(含已达 max-retries 的死态);不改服务端 ring/SSE 契约。
- **DoD**:vitest 定向测试先红后绿(gap 超 ring 后重连回填齐全 / 死态页面 visibility 复活);SPA 既有 vitest 不退(392+);writeback 绿;落 `dev` 汇入 PR #166。
- **验证**:vitest 392→395(重连 epoch 递增 / 耗尽死流 visibility 复活 / history-only 漏帧回填不重复);规划 diff review 通过(epoch≤1 跳过双拉、请求竞态守卫、fold 屏障=回填时 buffer 长度、监听器 teardown);规划独立复跑 vitest 395/395。ff 合入 dev(e3dfe5c)。

### IM-MIRROR-1 web 会话异步收尾镜像到 owner IM
- **状态**:完成(4f1a45a) · **冲突域**:`crates/ccteam-im(gateway pump ANSWER)` · **入口**:codex dev 会话 s119(规划派工 + 把关;review 裁决收紧 root-only:委派子会话不镜像,`parent_sid.is_some()` 即跳过)
- **背景**:owner 实测同上:web 建会话(reply_to=web)在人离开 web 时完成异步任务(委派完成通知/后台唤醒触发的收尾 turn),IM 零推送——「web 建、手机驱动」只通入站未通出站。owner 2026-07-23「一块修复」拍板。
- **规格**:pump ANSWER 侧:reply_to.channel=="web" 且该 turn **非用户亲发**(gateway 记 per-session turn 起源:用户 chat submit=user;委派通知注入/无 submit 自发=internal)→ 完成时把最终 answer **另发一条**到 owner IM:admin 池(`user:web-api`)→ 全局 telegram 首个 allowlisted chat;tenant → 复用 `user_delivery_target`(EXT-MCP-1);无可用 IM = 静默跳过。**每 turn 至多一条**(沿 FB-1 turn 边界),用户亲发 turn 零镜像(零骚扰);web 原路 ANSWER/账本零变。
- **DoD**:定向测试先红后绿(internal-origin 镜像一条 / user-origin 零镜像 / 无 IM 配置静默);`make test-baseline` ≥1615 只增;clippy 0;fmt 干净;writeback 绿;落 `dev` 汇入 PR #166。
- **验证**:ccteam-im 475→481(admin 池 internal turn 镜像一条 / web 亲发零镜像 / 无 telegram 配置静默 / tenant 走 `user_delivery_target` / **委派子会话零镜像**回归);规划 diff review 通过(起源在 submit 缝标记:`submit_web_sid`→User、`submit_to_sid`/委派注入→Internal;turn 边界消费;delivery-only 通道不回流 SSE;review 抓到子会话刷屏风险 → 裁决 root-only 已落);基线并入三卡总跑 1623/0。ff 合入 dev(4f1a45a)。

### IM-STATUS-1 IM 会话观测面:`/status` 外显子会话 + `/sessions` 带 activity + 文字/按钮互补
- **状态**:完成(cc0c5cf) · **冲突域**:`crates/ccteam-im(gateway /status·/sessions 渲染)` · **入口**:codex dev 会话 s119(IM-MIRROR-1 落地后同 worktree 串行追派)
- **验证**:三卡合并验收(规划独立复跑):fmt 干净;clippy 0 warnings;确定性基线 **1623/0**(1615→1619→1623 只增);vitest **395/395**(392+3);`pty_ws_test` 3 红 = 已登记 env-flake 族不变;diff review 通过(root-only 门 `parent_sid.is_some()` 跳过 / web 控制面 activity 渲染前 continue 零变化 / `/sessions` activity 与 session_list 同源分类器 / 按钮纯动作面 payload 不变);ff 合入 dev(e3dfe5c+4f1a45a+cc0c5cf)。
- **背景**:owner 实测(2026-07-23)三连:① s117 委派 s119 干活期间 `/status` 当前会话卡只显示 s117 🟢 idle,子会话 working 不可见——「父 idle」信号误导;② `/sessions` 树形行(└─)有 facets/ctx/title 但**无 activity**(session_list 工具面早有 working/idle,IM 文字未渲染);③ `/sessions` 的文字列表与 inline 按钮内容重复。
- **规格**:① `/status` 当前会话块追加子会话区:直接子会话(`parent_sid == 当前 sid`,live)的 `sid · vendor · activity(🟡 working/🟢 idle)· title(截断)`,更深后代给计数,无子会话零输出;② `/sessions` 每行加 activity 标记(与 session_list 的 working/idle/stale/stuck 同源同义);③ `/sessions` 文字=信息面(树/facets/activity/title),按钮=动作面(紧凑 sid 切换目标,不复述信息)——互补不重复;数据全走既有 sessions map/delegation 账本,不新增状态面。
- **DoD**:定向测试先红后绿(working 子会话行外显 / /sessions 行含 activity / 按钮 label 不含重复 facets);`make test-baseline` 只增;clippy 0;fmt 干净;writeback 绿;落 `dev` 汇入 PR #166。

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

### P1-2 session_collect 游标去重
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_collect MCP)` · **建议入口**:dev 会话
- **背景**:collect 会重复返回已读段(v0.9.2 遗留 P1)。坐标开工时核现值。
- **规格**:collect 游标语义去重;`max_chars` 限幅与账本指针行为零碰。
- **DoD**:新定向测试先红后绿;`make test` 基线只增;writeback 绿。

### P1-3 remove_test 对齐 v0.9.0 废-cto 语义
- **状态**:待排 · **冲突域**:`crates/ccteam-cli(tests/remove_test.rs)` · **建议入口**:dev 会话
- **背景**:`t03_purge_clears_ccteam_footprint_only` + `t17_project_rm_purge_via_group` 干净 origin/main 确定性红(V095 偏差 2 上报,规划复核复现):测试仍期望 purge 删「seeded cto.md」,但 v0.9.0 废 cto 后红线明确 `.claude/agents/cto.md` = 用户文件不删不改 —— **测试语义未跟,代码行为是对的**。CI 不跑测试故长期未拦(见 P2-1)。
- **规格**:改两测试期望为「purge 保留 cto.md(用户 role 文件)」;顺带核 purge 实现与 §三红线一致,不改产品行为。
- **DoD**:t03/t17 绿;`make test` 基线只增;writeback 绿。

### V094 npm 分发 · daemon 管理 · 自更新
- **状态**:gated(owner 2026-07-17 暂缓,v0.9.5 先行) · **冲突域**:`install.sh + crates/ccteam-cli + Makefile` · **建议入口**:版本波(doc-first)
- **背景**:PRD 已成文 `docs-local/versions/v0-9-4/prd.md`(DRAFT)。2026-07-22 起其 daemon/update 范围由 V097 PRD 承接深化,本卡剩余主体 = npm 分发面(拍板时二者收敛)。
- **规格**:占位指针卡,**不含实现授权**;拍板后由规划拆 wave 卡替换本卡。
- **DoD**:—(gated)

### P2-1 CI 增确定性测试 job
- **状态**:待排 · **冲突域**:`.github/workflows/` · **建议入口**:规划(控制)会话(治理面;改 workflow 须 SSH push,§六)
- **背景**:V095 复核发现 `check.yml` 只跑 fmt + clippy,**测试完全不在 CI** —— 基线只增当前全靠会话自律 + 复核(P1-3 的测试陈化即因此漏网)。确定性口径(`--lib`)本就为免 env-flake 设计,适合上 CI。
- **规格**:加 job `cargo test --workspace --exclude ccteam-web --lib --locked`;**不**上 web/e2e(env 依赖);过门后同步 `.loop/verify/README.md` 门禁地图。
- **DoD**:CI 三 job 绿;writeback 绿。

## 历史波指针

- v0.9.7(daemon Codex pid-detach 重构 + `ccteam update`,PR #165 `825ae7d`)· v0.9.2 及此前 → `.loop/history.md`(每版一行)+ `git log` + `docs-local/versions/`(gitignored 详档)
