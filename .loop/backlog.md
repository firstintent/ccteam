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

### A2A-W5 A2A 线收尾:三场景真机 smoke + README/usage 重写
- **状态**:待排 · **冲突域**:`README.md + docs/`(smoke 零代码)· **建议入口**:规划(控制)会话(涉治理面写权)
- **背景**:v0.9.0–0.9.2 A2A 底座已落,W5 是 ship gate 前最后一步;hub 示例配方已落(`cct-codex`/`cct-grok` skill + `team-brain` agent,grok 跨模型 review 已跑通)。
- **规格**:① 三场景真机 smoke(单机委派 / 跨 vendor / 卫星跨机),结果留痕 `docs-local/versions/`;② root README + `docs/usage.md` 把 A2A 融入当前能力描述(README 英文、不写版本时间轴,§三红线)。
- **DoD**:三场景各一次全链路通过记录;docs-only 面走最低门(fmt + writeback);writeback 绿。

### P1-1 codex turn 粒度折叠
- **状态**:待排 · **冲突域**:`crates/ccteam-harness(codex adapter)` · **建议入口**:dev 会话
- **背景**:codex 叙述消息被当独立 turn 记账/展示,应折叠进所属 turn(v0.9.2 遗留 P1)。坐标开工时核现值。
- **规格**:折叠 codex 叙述消息进所属 turn;不改 `CanonicalEvent` schema 语义(schema 权威 = `harness/progress_bridge`)。
- **DoD**:新定向测试先造缺陷态红、后修绿(证有牙,留痕验证段);`make test` 基线只增;writeback 绿。

### P1-2 session_collect 游标去重
- **状态**:待排 · **冲突域**:`crates/ccteam-im(session_collect MCP)` · **建议入口**:dev 会话
- **背景**:collect 会重复返回已读段(v0.9.2 遗留 P1)。坐标开工时核现值。
- **规格**:collect 游标语义去重;`max_chars` 限幅与账本指针行为零碰。
- **DoD**:新定向测试先红后绿;`make test` 基线只增;writeback 绿。

### V095 kimi-code 第五 vendor harness 集成
- **状态**:完成(d9e32e8) · **冲突域**:`crates/ + crates/ccteam-web/web + README.md` · **建议入口**:dev 会话(owner 钦点 kimi-code 单会话一口气)
- **背景**:owner 2026-07-17 定向(v0.9.4 暂缓,本版先行)。PRD 自包含(两侧源码坐标已内嵌)= `docs-local/versions/v0-9-5/prd.md`;模板 = grok/opencode 薄壳 ACP vendor。
- **规格**:PRD F1–F9。协议钉 `kimi acp` 长驻 stdio(terminal 冻结红线);复用 `execution/acp/*` 通用引擎 + `mcp_config::acp_mcp_servers_http`;全局注册写 `~/.kimi-code/mcp.json`;cost `None` 仿 Opencode;roleless-only;remote NotImplemented。
- **卡面授权路径**:`docs/usage.md`(仅本卡;AGENTS.md / docs 其余 = 规划会话收口时改,dev 偏差申报)。
- **DoD**:PRD §6 全五条 —— `make gate` 基线只增 · 假件测试先红后绿 · 真机 smoke 留痕 · doctor 不退步 · version bump `0.9.5`(tag/部署 HELD);writeback 绿。
- **验证**(kimi·2026-07-17,实现 d9e32e8):
  - 假件先红后绿:`git stash`(缺陷态,kimi 面不存在)→ `cargo test -p ccteam-harness --test kimi_acp_test` 编译红(E0432/E0433/E0599 共 6 错);`git stash pop` → **11/11 绿**(留痕 `docs-local/versions/v0-9-5/test-red-green.log`)。
  - 基线只增:lib `cargo test --workspace --exclude ccteam-web --lib` = **1433/0**(1408→1433,+25);vitest **378/0**(376→378,+2);ccteam-web `--no-fail-fast` = **311 过 + 3 失败**(3 个 = `ws_*` pipe-pane env-flake 族,AGENTS.md §六);clippy 0 warnings;`cargo fmt --all -- --check` 干净;`.loop/verify/writeback.sh` GREEN;`ccteam doctor --verify-mcp` PASS(8/8 无 STUB)。
  - 全量对照:`cargo test --workspace --exclude ccteam-web --no-fail-fast` = 2287 过 / 6 失败,**6 个全部在干净 origin/main 同机复测同红**(env-flake 族;证据 = 本卡偏差段 + `docs-local/versions/v0-9-5/smoke/gate-evidence.md`)。
  - 真机 smoke(daemon 重启到 0.9.5,留痕 `docs-local/versions/v0-9-5/smoke/smoke-run.log`):REST spawn kimi = s44 → dispatch → collect 收 `KIMI_SMOKE_OK`;`/model` picker 出**真 availableModels**(K2.7 Coding / K2.7 Coding Highspeed / K3)→ resolve 切 `kimi-code/k3`(`/status` 证实已切);stop;`ccteam config mcp` 五 vendor 注册含 `~/.kimi-code/mcp.json`(headers map 形态,0600);doctor `MCP (kimi)` PASS;**kimi 主会话经全局注册调通 `mcp__ccteam__status`**。
- **偏差**:
  1. 治理面未动(超卡面授权,按规则停手申报):AGENTS.md §〇/§四 vendor 提及、`docs/orchestration.md(+cn)`、`docs/usage-cn.md` 的 vendor 清单仍写四 vendor,请规划收口时同步五 vendor;`docs/usage.md` + `README.md` 已按卡面授权更新。
  2. 观察(非本卡引入,未动):`remove_test` 的 `t03_purge_clears_ccteam_footprint_only` + `t17_project_rm_purge_via_group` 在本机**干净 origin/main 亦确定性红**(cto.md purge 期望,疑 v0.9.0 废 cto 后测试未跟);建议列入 env-flake 族或另立卡清理。
  3. 观察(运维面):Makefile systemd unit `Environment=PATH` 未含 `~/.kimi-code/bin` —— daemon 找不到 kimi;本机已按其余 vendor 同款软链 `~/.local/bin/kimi` 解决。建议规划评估 unit 注释提及 kimi 路径。
  4. 实现决策(卡面授权内,与 PRD 勘察偏差一处):真机 kimi ACP 的模型目录走 **`configOptions`**(opencode 同款,非 PRD 字面 "availableModels")——通用引擎 `pluck_model_info` 的 configOptions 路径原样复用,零新代码;`/model` 切换走 `session/set_model {sessionId, modelId}`(真机验证);effort 轴按 PRD 非目标未接(SPA/REST 两侧 kimi effort 均置 null)。
  经验:SPA Sidebar 每工作区有 WS_SHOW 行数上限,扩 vendor 测试行时须跨 project 摆放,否则被折叠。

### V094 npm 分发 · daemon 管理 · 自更新
- **状态**:gated(owner 2026-07-17 暂缓,v0.9.5 先行) · **冲突域**:`install.sh + crates/ccteam-cli + Makefile` · **建议入口**:版本波(doc-first)
- **背景**:PRD 已成文 `docs-local/versions/v0-9-4/prd.md`(DRAFT)。
- **规格**:占位指针卡,**不含实现授权**;拍板后由规划拆 wave 卡替换本卡。
- **DoD**:—(gated)

## 历史波指针

- v0.9.2 及此前 → `.loop/history.md`(每版一行)+ `git log` + `docs-local/versions/`(gitignored 详档)
