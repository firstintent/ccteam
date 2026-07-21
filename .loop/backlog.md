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

### V097-W12 daemon 生命周期核 + systemd 退场(PRD W1+W2)
- **状态**:进行中(规划调度 subagent·2026-07-22) · **冲突域**:`crates/ccteam-core + crates/ccteam-cli + install.sh + Makefile` · **入口**:规划会话调度(owner 直驱 dev+PR)
- **背景**:owner 2026-07-22 拍板 V097 进入开发(废 systemd + D1–D8 按 PRD v4 默认,登记 `state.md` 人工门)。规格 SoT = `docs-local/versions/v0-9-7/prd.md` §三 F1/F2/F4 + §四 Codex 借鉴地图(pid.rs+测试直接搬,Apache-2.0 attribution 进 LICENSES.md)。工作树 `/tmp/ccteam-v097`(branch `dev`),PR 目标 `main`,tag/部署 HELD。
- **规格**:F1 生命周期核(pid JSON + `daemon.lock` + 双判定 + versioned probe)+ F2 `daemon start/stop/restart/status/logs`(全 `--json`;trigger-file 全链退役;`ccteam stop` 委托 alias;前台 start 加 probe 实例守卫)+ F4 legacy 自动接管(Rust 单一实现;install.sh 只检测+调用;指纹白名单,手写 unit 永不代删)+ W2 删 unit 面(Makefile/install.sh 零 unit 文本;`make install` = build+`daemon restart`)+ doctor 残留 WARN。
- **DoD**:PRD 验收 A1–A6;确定性 lib 基线只增(≥1472/0);clippy 0;fmt 干净;writeback 绿。

### V097-W3 update + InstallChannel + 版本外显(PRD W3)
- **状态**:待排 · **冲突域**:`crates/ccteam-core + crates/ccteam-cli + install.sh + docs/` · **建议入口**:规划会话调度
- **背景**:同 V097-W12,在其之上叠加(**依赖 W12,同域串行**);PRD §三 F3 + §六 W3。P1-4(unit PATH 补 kimi)已按 D7 随 unit 面删除而取代,细节见 state.md 人工门 + V097-W12 卡。
- **规格**:InstallChannel(env 预留/marker/路径启发)+ `ccteam update`(standalone 臂转发非交互 install.sh、source 臂只指引 + 升级重启合同:in-flight 等 5min/`--now`/`--no-restart` + 版本核对)+ `state/version.json` 轻量检查(≥20h,lazy,不进 daemon loop)+ doctor updates 段 + fleet 版本偏差外显 + workspace 版本 bump `0.9.7` + docs 同步(usage/README/tech-design 指针)。
- **DoD**:PRD 验收 A7–A10;基线只增;writeback 绿;PR 就绪。

### P2-1 CI 增确定性测试 job
- **状态**:待排 · **冲突域**:`.github/workflows/` · **建议入口**:规划(控制)会话(治理面;改 workflow 须 SSH push,§六)
- **背景**:V095 复核发现 `check.yml` 只跑 fmt + clippy,**测试完全不在 CI** —— 基线只增当前全靠会话自律 + 复核(P1-3 的测试陈化即因此漏网)。确定性口径(`--lib`)本就为免 env-flake 设计,适合上 CI。
- **规格**:加 job `cargo test --workspace --exclude ccteam-web --lib --locked`;**不**上 web/e2e(env 依赖);过门后同步 `.loop/verify/README.md` 门禁地图。
- **DoD**:CI 三 job 绿;writeback 绿。

## 历史波指针

- v0.9.2 及此前 → `.loop/history.md`(每版一行)+ `git log` + `docs-local/versions/`(gitignored 详档)
