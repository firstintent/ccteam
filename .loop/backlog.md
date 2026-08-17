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

### WEB-TS-1 web 聊天消息时间戳(v0.10.2 N1)★ W1
- **状态**:完成(c07ad2a3) · **冲突域**:`crates/ccteam-web/src/routes/sessions_api.rs(session_event_payload)+ crates/ccteam-web/web/src(chatTranscript.ts · hooks/useSessionEvents.ts · SessionView.tsx 气泡渲染段)` · **建议入口**:subagent(briefing 自包含)
- **验证**:2026-08-17 coder subagent 交付,规划收口(rebase+ff):payload additive `ts`(chrono Utc,RFC3339 与 turns.jsonl 同形同钟,定向单测 `session_event_carries_server_ts`);`TranscriptRow.ts` history/SSE 两路都填;`RowTime` 本地 HH:MM + title 完整时间,移动端零横向挤压。vitest 合并树 658/658 · tsc/eslint 绿 · `cargo test -p ccteam-web --lib` 164/0。**偏差**:无;judgment call = ts 取 payload 构建时刻(GatewayEvent 本体无 ts,加它要碰 ccteam-im 出卡域),与 turns 写入亚秒级差。
- **背景**:需求 SoT = `docs-local/versions/v0-10-2/README.md` N1(gitignored,briefing 内嵌全文)。数据链路本来通:turns.jsonl 每行有 ts、REST 历史透传 ts,但前端 `TranscriptRow` 丢弃、SSE 实时帧无 ts 字段。
- **规格**:① 后端 `session_event_payload`(sessions_api.rs:2012-2018)additive 加 `ts`(与 turns.jsonl 同源的服务端时间,禁前端 Date.now);② `TranscriptRow` 加 ts,`historyToRows`/`eventToRow` 都填;③ SessionView 气泡渲染小字 `HH:MM`(本地时区,title 给完整时间),移动端不挤压;④ additive 不破坏旧前端。
- **DoD**:历史与实时消息都显时间;刷新后同一消息时间不变;vitest/tsc/eslint 绿;`make test-baseline` 只增(1928/0 起);clippy 0;fmt 干净;writeback 绿。

### WEB-TREE-1 团队→拓扑按项目折叠(v0.10.2 N2)★ W1
- **状态**:完成(cbd4e12d) · **冲突域**:`crates/ccteam-web/web/src(pages/AgentsView.tsx · lib/agentsTree.ts)` · **建议入口**:subagent(与 WEB-NAV-1 同只,两 commit)
- **验证**:2026-08-17 coder subagent 交付:`groupDelegationTrees` 第三参 `collapsedProjects`(折叠 = rows 空但 slug/live/total 计数保留,与 per-sid collapsed 正交);localStorage `ccteam.agents.collapsedProjects.v1`(损坏回落全展开);toggle 带 aria-expanded/testid;vitest +4(纯函数 2 + 视图 2),合并树门禁见 W1 收口行。**偏差**:无。
- **背景**:N2。`AgentsTree` 已按项目分组(AgentsView.tsx:138-216),既有折叠仅到会话子树级(`collapsed: Set<sid>`,agentsTree.ts:30-45)。
- **规格**:`collapsedProjects: Set<slug>` state(与 Set<sid> 并存不混);项目 header(AgentsView.tsx:139-143)加展开/收起 toggle,折叠隐藏该项目全部会话行、header 计数保留;默认全展开;localStorage 持久化;过滤逻辑落 agentsTree.ts 纯函数(保持「React 只持集合、过滤纯函数」风格);纯展示层不动数据获取。
- **DoD**:折叠/展开正确、计数在、刷新后折叠态保留;vitest 覆盖纯函数;`make web-check` 绿;writeback 绿。

### WEB-NAV-1 logo 回首页 + Sidebar 底部换序(v0.10.2 N7+N8)★ W1
- **状态**:完成(5b382e29) · **冲突域**:`crates/ccteam-web/web/src(components/Sidebar.tsx · components/Logo.tsx)` · **建议入口**:subagent(与 WEB-TREE-1 同只,两 commit)
- **验证**:2026-08-17 coder subagent 交付:Sidebar 新必需 prop `onOpenHome`(ChatConsole 接线 navigate("/")),两态 logo 可点(testid `side-home`/`side-home-rail`);side-bottom 用户行在上、设置最底,rail 镜像,顺序注释同步;i18n `home` 键双语;vitest +3,ChatConsole.shell rail 顺序断言更新。**偏差**:tooltip 用 SVG `<title>` 子元素(React SVG typings 不收 title 属性)。
- **背景**:N7/N8。展开态 logo(Sidebar.tsx:291)无点击;折叠态 logo(:596)点击=展开(与 :597 Chevron 冗余);`side-bottom`(:574-591)现状设置在上、用户行在下;折叠 rail 同序(两态顺序一致是既有不变量,:594 注释)。
- **规格**:① 两态 logo 点击 → navigate `/`(即 /app/ 首页),折叠态展开职能留 Chevron;cursor pointer + title + data-testid。② side-bottom 换序:用户行在上、设置最底;折叠 rail 同步换;两态一致注释同步改。
- **DoD**:任意页点 logo 回 `/`;折叠态仍能 Chevron 展开;两态顺序断言更新;vitest 绿;writeback 绿。

### WEB-DSH-1 DSH 菜单 iframe keep-alive(v0.10.2 N9)★ W1
- **状态**:完成(06faf815) · **冲突域**:`crates/ccteam-web/web/src(App.tsx · ChatConsole.tsx shell · pages/DshView.tsx)` · **建议入口**:subagent
- **验证**:2026-08-17 coder subagent 交付:新 `dshStore.ts`(useSyncExternalStore,visited 懒门 + 单一 status 源消重复轮询)+ `DshFrameHost.tsx`(ChatConsole `<main>` 内、view switch 外常驻,off-/dsh 只 `hidden`);stop→start 经 src=null 自然换新 iframe(预期 reload)。jsdom 真 ChatConsole 导航测试:iframe 节点身份跨 `/`→`/dsh`→`/`→`/dsh` 不变 + hidden 切换 + stop→start 新节点;未访问零 dsh fetch。vitest 子树 647(60 文件)。**真机 DoD 余项**:浏览器手感复核随版本 dogfood。**偏差**:无。
- **背景**:N9。`/dsh` 普通路由切走即 unmount DshView → iframe 销毁 → 切回整页重载。修法 = 挂载一次 + CSS 隐藏,不引第三方库。
- **规格**:iframe 宿主提升到常驻 shell(ChatConsole 布局层),首次访问 `/dsh` 才挂载(模块级 flag,未访问零请求),路由切换只 display/visibility 不 unmount;DshView 本体(status head/空态)仍随路由渲染;`embedSrc(status)` 为 null 时无 iframe 可保;stop→start 后 reload 属预期。**不**顺势改 TerminalView。
- **DoD**:真机/Playwright:加载完切走再切回,DSH 内草稿/滚动原样、网络面板无资产重拉;未访问零 dsh 请求;vitest 覆盖「路由切换不 unmount」;`make web-check` 绿;writeback 绿。

### WEB-STATUS-1 会话页状态点实时化(v0.10.2 N3)★ W2
- **状态**:完成(7948e17f) · **冲突域**:`crates/ccteam-web/web/src(pages/SessionView.tsx 头部 · hooks/useSessionEvents.ts · components/ChatConsole.tsx)` · **建议入口**:subagent(W1 合入后开工,SessionView 与 WEB-TS-1 串行)
- **验证**:2026-08-17 coder subagent 交付(基 06faf815):`SessionEvent` 增 state/reason 解析 + 纯函数 `foldSessionLiveness`(evicted/stopped→off,renamed/identity_degraded 无意见);headDot = busy 琥珀 › 会话活性(session.status 基 + lifecycle 折);连接失败拆成独立红色 conn-dot(i18n `connLost` 双语)。vitest +7;合并后 `make web-check` **665/665** EXIT 0。**同形扫一遍**:rail 会话行本无状态点,conv-head 是唯一病点;HostsView 等点 = host/daemon 级出域。**诚实留痕**:显式 stop 与冷 resume 目前不发 lifecycle 帧(wire 词表仅 evicted/renamed/identity_degraded),off→on 靠 rail REST reconcile;词表扩充列后续候选。**真机 DoD 余项**:挤停实测随 dogfood。**偏差**:无。
- **背景**:N3 根因:SessionView.tsx:413-419 headDot 数据源 = per-sid SSE 连接态(connected 恒绿),会话被挤停照样绿;服务端本在发 `session_lifecycle` 帧(sessions_api.rs:2056-2059 带 state/reason)但 eventToRow 丢弃、SessionView 不消费;`session.status` prop 现成未用。拓扑页对 = REST 快照 + 全局 SSE。
- **规格**:headDot 换数据源:初值 `session.status`,之后消费 per-sid SSE `session_lifecycle` 即时更新(live=绿/off=灰;busy 琥珀保留本地推导;连接错误红点保留为连接信号,可与状态点分开表达,实现从简);范式参考 ChatConsole.tsx:198 `useAgentsEvents(true,"session_lifecycle")` + lib/lifecycleReconciler;**同形扫一遍**:rail 列表会话项状态点若同病一并修;服务端零改动。
- **DoD**:挤停/结束后事件到达即变灰(不刷新页面);live 绿、本页 turn 琥珀;与拓扑页同会话状态一致;vitest 覆盖 lifecycle 帧驱动迁移;`make web-check` 绿;writeback 绿。

### VENDOR-INSTALL-1 Ops & Hosts vendor 一键安装/更新(v0.10.2 N5)★ W2
- **状态**:完成(cae6482b) · **冲突域**:`crates/ccteam-core(host_registry.rs)+ crates/ccteam-web/src/routes(hosts.rs 等新端点)+ crates/ccteam-web/web/src(pages/HostsView.tsx · lib/hostsApi.ts)` · **建议入口**:subagent(与 VENDOR-QUOTA-1 同只串行,两 commit)
- **验证**:2026-08-17 coder subagent 交付:`AgentProbeSpec.install_recipe` 固定 argv 表(npm 系 5 家,kimi/pi None + `manual_install_url`);新 `routes/vendor_install.rs` = `POST /api/v1/hosts/{host}/vendors/{vendor}/install`(202+job,同 vendor 去重,404/400 矩阵)+ `GET …/install/{job_id}`,双端点 deny_non_admin,detached task + kill_on_drop + 10min 超时 + 24 行有界输出尾,`Command` 直起不过 shell;三处 "never installs" 文案改写(hosts.rs hint / HostsView 头注释 / vendor_panel.rs + spawn_spec.rs 一处陈旧引用);SPA 三态按钮(Install / `Update → <latest>` / 无)admin-only 渲染 + npm 缺失置灰 + 成功后 re-probe。定向:配方表 argv 钉死单测 + `vendor_install_test` 4/0(fake-npm 快乐路 + 去重 + EACCES 尾 + 租户 403)+ openapi drift 集更新(85 ops)。**偏差**:进行中行内只显最后一行输出(hover 给全尾),保行紧凑。**真机 DoD 余项**:真装/升一家随版本 dogfood。
- **背景**:N5。**owner 显式推翻既有立场** "ccteam never installs a CLI for you"(hosts.rs:177-179 / HostsView.tsx:13 / vendor_panel.rs:403-407 三处文案随卡改写);红线本体(不 vendor 二进制)不动。npm 包映射现成在 `web/src/lib/vendorLatest.ts`(claude/codex/grok/opencode/dsh 有,kimi/pi 无);admin 门 = `deny_non_admin`(auth.rs:220);受管子进程范式 = dsh_web.rs;daemon 无「REST 触发子进程」先例,本卡收敛引入。
- **规格**:① `AGENT_PROBE_SPECS` 每 vendor 增 `install_recipe: Option<&[&str]>`(npm 系 = `npm install -g <pkg>@latest`;kimi/pi = None);`Command::new(argv[0]).args(...)` 直起**不过 shell**,请求只带 vendor 名服务端查表,无自由命令面。② `POST /api/v1/hosts/local/vendors/{vendor}/install`(deny_non_admin)→ 202 + job id;`GET …/install/{job}` 轮询(running/exit/输出尾);同 vendor 并发去重;超时(~10min)强杀记失败;spawn_blocking/detached 仿 dsh_web。③ 仅本机(远端卫星行无按钮)。④ VendorManageRow 按钮三态:未装=安装/已装且 outdated=更新/最新不渲染;仅 `useMe().isAdmin` 渲染(真门在后端 403);行内展开输出尾+终态,成功后 `getHostDetail(host, refresh=true)`。⑤ hint 三处改写新口径。⑥ npm 不在 PATH 置灰+hint;权限不足如实报 stderr tail,不提权。
- **DoD**:真机装/升一个 npm 系 vendor 全链路;非 admin POST 403;kimi/pi 无按钮;伪造 vendor 名/参数 → 4xx,argv 恒等于配方表(定向测试);基线只增、clippy 0、vitest 三态;writeback 绿。

### VENDOR-QUOTA-1 Ops & Hosts vendor 额度展示(v0.10.2 N6)★ W2
- **状态**:完成(28c788c4) · **冲突域**:同 VENDOR-INSTALL-1(同只 subagent 串行) · **建议入口**:subagent
- **验证**:2026-08-17 coder subagent 交付:归一模型 `VendorQuota`/`QuotaWindow` + `QuotaProbeKind` 注册表字段(`core/src/vendor_quota.rs`,14 fixture 测);`GET /api/v1/vendors/quota` 独立 admin 端点(并发探测各 8s、per-vendor 5min 缓存、失败隔离);**活探测器 = claude/codex/kimi**(endpoint/header/shape 对 references 源码核实;kimi 凭据位 `~/.kimi-code/credentials/kimi-code.json` 磁盘实证);**grok = Unavailable stub**(四头耦合值无法从 `~/.grok/auth.json` 干净推导,不伪造请求,代码注释留据);opencode/pi/dsh = None。SPA 迷你条(5h/周 + plan badge + 本地相对时间),not_subscription/unavailable 不渲染。定向:租户 403 + 环境隔离 shape/缓存测;vitest 合并树 685/685(+20)。**偏差**:codex parser 兼容 `rate_limits` 包裹与扁平两种拼写(codex-rs `#[serde(flatten)]` 实证);凭据文件缺失映射 not_subscription,传输失败才 unavailable。
- **W2 收口(规划,合并序 STATUS→INSTALL→QUOTA 全 rebase+ff,head `28c788c4`)**:`cargo fmt --all -- --check` PASS;`make clippy` 0;`make test-baseline` **1947/0**(1931→+16);`cargo test -p ccteam-web --lib` **174/0** + `vendor_install_test` 4/0 + `vendor_quota_test` 2/0;`make web-check` vitest **685/685**(61 文件,665→+20)+ tsc 绿;writeback 绿。
- **背景**:N6,各 vendor 额度面已全部实锤(需求文档表格):claude `GET api.anthropic.com/api/oauth/usage`(Bearer=~/.claude/.credentials.json OAuth token)→ five_hour/seven_day.{utilization%,resets_at};codex `GET chatgpt.com/backend-api/wham/usage`(Bearer=~/.codex/auth.json + ChatGPT-Account-Id)→ primary(5h)/secondary(weekly).{used_percent,reset_at}+plan_type;kimi `GET api.kimi.com/coding/v1/usages`(managed OAuth)→ usage(weekly)+limits[] 300min 窗(5h)各带 resetTime;grok `{cli-chat-proxy}/billing?format=credits`(四头)→ creditUsagePercent+currentPeriod.end+subscriptionTier(无 5h);opencode/pi 无面;dsh 余额制候选不做。
- **规格**:① 归一模型 `VendorQuota{state: Available{plan, windows:Vec<QuotaWindow{kind:5h|weekly|monthly, used_percent, resets_at}>} | NotSubscription | Unavailable}` + per-vendor `quota_probe` 注册表(host_registry 旁,与 AGENT_PROBE_SPECS 同哲学,新 vendor 加一行 UI 零改)。② `GET /api/v1/vendors/quota` additive 独立端点(不塞 host detail),**admin-only**,per-vendor 缓存 TTL ~5min(仿 probe_bin_cached),页面加载并行拉取,配额面挂不影响主数据。③ 凭据**只读**;401/超时/API-key 登录 → Unavailable 不渲染不报错;不做 OAuth refresh。④ UI:行内迷你条 `5h ▓▓░ 42% · 3h12m 后重置` / `周 …`,单窗 vendor 只一条,plan 名小 badge,本地时区相对化,移动端不挤压。⑤ 被动流(claude rate_limit_event/codex headers)不做;dsh 余额、opencode 429 抠字 = 候选不做。
- **DoD**:本机已登录订阅 vendor 行内显示与 vendor 自家 /usage 一致;API-key/未登录行无额度区无报错;非 admin 403;每探测器 fake-HTTP 定向测试(shape 解析 + 401/超时降级);TTL 生效断言;基线只增、clippy 0、vitest 三态;writeback 绿。

### CLI-HELP-1 `ccteam --help` 文案整洁化(v0.10.2 N4)★ W1
- **状态**:完成(485da277) · **冲突域**:`crates/ccteam-cli` · **建议入口**:subagent
- **验证**:2026-08-17 coder subagent 交付(5 文件 +284/−556 纯文案):顶层 about = "Multi-harness agent team bridge and governance layer";57 个 help 页脚本化扫描全 ≤100 字符、零版本字面量;`grep '///.*[vV]0\.'` 全 crate 零命中(版本注记只存 `//` 实现注释);cli lib 94/94 + cli_surface 等定向测绿;`web_subcommand_test::ccteam_web_serves_health_then_exits_when_killed` 红 = pristine base 复跑同红,登记环境族非本卡。**偏差**:同形扫按字面执行(内部 fn/test doc 的版本字面量也清);无版本号的 feature-id(PRD F3.3 等)保留,非用户面。
- **W1 收口(规划,合并序 CLI-HELP→TS→TREE+NAV→DSH→STATUS 全 rebase+ff,head `7948e17f`)**:`cargo fmt --all -- --check` PASS;`make clippy` 0 warnings;`make test-baseline` **1931/0**(起始口径 1931/0 —— 测量时撞登记 echo 竞态族 `turn_answer_carries_context_echo_for_focused_im_session` 一红,隔离复跑绿,非回归);`cargo test -p ccteam-web --lib` 164/0;`make web-check` vitest **665/665**(60 文件,640→+25)+ tsc 绿;writeback 绿。周期 draft PR = #185。
- **背景**:N4。现状:多条命令/选项描述是多段开发笔记(Init 5 段/Update 3 段/Start 每选项 3-4 行);版本号写进用户面(`--home`「v0.8.20 —…」main.rs:55、`--owner`「v0.8.20 F1:…」:101、`--no-clipboard`「V0.4.6 F88—…」:145、Status「V0.4.1:…」:152);内部开发史引用数十处(Item 4/W3/W4a/v0.9 T5/Track D);顶层 about「built on Claude Code」名不副实(七 vendor)。
- **规格**:① 全部用户可见命令/选项描述 ≤100 字符、首段一行(clap 短 help 语义)、简洁用户导向;② **零版本号字面量**(v0.x.y/V0.x 全清);③ 范围:顶层 + project/session/role/skill/host/daemon/config/init/start/stop/status/update 全部子命令与选项;internal(hide)顺手清但不强求 100 字符;④ 顶层 about 改中性现行描述(对齐 README 口径,如 "Multi-harness agent team bridge and governance layer");⑤ 被删开发史注记直接删(git history/docs-local 是其家),有价值行为说明改 `//` 注释留实现处;⑥ grep 全 crate `///` 里 `v0\.|V0\.` 同形扫净。
- **DoD**:`ccteam --help` 及各子命令 --help 全部描述 ≤100 字符、零版本号;顶层一屏每命令一行;行为零变更(纯文案);`make test-baseline` 只增(1928/0 起)、clippy 0、fmt 干净;writeback 绿。

### PERF-REVIEW-FIX-1 perf-v1 review 定性后的修复批(owner「review and fix」2026-08-17)
- **状态**:完成(6ee84bf) · **冲突域**:`crates/ccteam-web/web(SessionView/ChatConsole/stores)+ crates/ccteam-web/src/routes(ETag 响应头)` · **建议入口**:codex 委派(单只,owner 限 ≤1 并发)
- **背景**:owner 令「review and fix」;review 舰队被叫停(以后禁用 code-review),已完成的 web 角扫描 8 条发现经规划逐条核代码定性:**4 伪**(external 500——per-file 容错在扫描器内;activity fallback 0——与 IM 同款且注释明写;ETag 双序列化——per-identity 304 正确性所需;error_code 标签微偏不修)、**2 微**(If-None-Match 不认弱验证器;lifecycle 环 indexOf 对象身份)、**2 实**(loadEarlier 无 epoch 守卫可致断层+游标错位;ETag 响应缺 `Cache-Control: private` 在共享缓存部署下可跨租户串看)+ **1 实**(hidden 期 SSE 断链后 focus 不补刷 session 列表,rail 陈旧)。
- **规格**:① SessionView `loadEarlier` 套用与 seed/reseed 相同的 `historyRequestRef` epoch 守卫,过期响应丢弃;② status/projects 的 ETag 响应统一加 `Cache-Control: private, no-cache`(304 revalidation 语义保留);③ visibilitychange→visible 时对已注册 slug 触发一次 debounced reconcile(补 SSE 盲窗);④ lifecycle 已消费指针改单调序号,不用对象身份 indexOf;⑤ If-None-Match 比对容忍 `W/` 前缀(`*` 不做)。
- **DoD**:各项 vitest/HTTP 断言;vitest/tsc/eslint 绿;Rust 面 fmt/clippy/基线只增(1928/0 起)。
- **验证**:2026-08-17 codex s445 交付 `6ee84bf`(五项全落:loadEarlier 复用 seed 同款 `historyRequestRef` epoch 双检;status/projects 的 200 与 304 全带 `Cache-Control: private, no-cache`;visible 恢复经既有 debounced reconciler 每 slug 一次;lifecycle 环 additive 单调序号;If-None-Match 逗号列表 + `W/` 前缀容忍)。规划收口(合并树):fmt PASS、clippy 0、序列化 baseline **1928/0**、`make web-check` vitest **640/640**(637→+3)+ tsc 绿、web 套件 406/1(登记 pty flake)、writeback 绿。 `make install` 撞非受管 daemon 时误报安装失败(owner 直驱 2026-08-17)
- **状态**:完成(9e6375e) · **冲突域**:`crates/ccteam-cli/src(daemon_cli.rs + main.rs clap)+ Makefile(install)` · **建议入口**:codex 委派
- **背景**:owner 在另一台机器 dev 分支 `make install` 报 Error 1——二进制已成功安装,但 install 目标的 `ccteam daemon restart` 对「非受管 daemon 占 socket」(前台 `ccteam start`/supervisor,owner 常态)按设计拒绝(`StopVerdict::RefusedNotManaged`,daemon_cli.rs 映射为 fail)→ make 把成功的安装打成失败。v0.9.7(`825ae7d6`)起即有。同形:`ccteam update` 的 RestartRefused 同样非零退出(update.rs:442,契约面,本卡不动、报 owner 裁决)。
- **规格**:`daemon restart` 加 `--if-managed` flag——非受管占 socket 时打响亮 drift 警告(明说「新装二进制未生效,需自行重启那只 daemon」)+ exit 0;受管/无 daemon 行为不变;真失败(stop 超时/spawn 错)仍非零。install 目标改用 `daemon restart --if-managed`。
- **DoD**:flag 三态测试(非受管→警告+exit0 / 受管→重启 / 无→启动);Makefile 改行;基线只增、clippy 0、fmt 干净。
- **验证**:2026-08-17 codex s444 交付 `9e6375e`(纯映射缝 `restart_command_action(outcome, if_managed) -> Emit|Fail`,单元测试 4/4;`--if-managed` 仅放宽 NotManaged 一臂 → `skippedNotManaged` + drift 警告「新装二进制未生效需自行重启」;裸 `daemon restart` 与 StopTimedOut/真失败契约一字不动;Makefile install 改用 `--if-managed`)。规划收口:fmt PASS、clippy 0、序列化 baseline **1928/0**(1924→+4)、cli 全量 198/1(唯一红 = 登记 web 子进程计时族,base 复现)、writeback 绿。

### PERF-V1-1 事件准入门(EventClass admission)★ W1
- **状态**:完成(11fc808) · **冲突域**:`crates/ccteam-harness/src/execution(progress_bridge.rs + 新 event_class + codex_app_server.rs)` · **建议入口**:codex 委派(规划 briefing 自包含)
- **背景**:excore.jsonl 149MB 中 ~85% 为全 null `codex_rate_limit`(相邻间隔低至 27µs,发射点 1:1 翻译无去重无节流);修在唯一写权威 `progress_bridge::append_event`(core 只 re-export,全 daemon 内写者天然过门)= 通用防爆炸,非 per-vendor 补丁。规格 SoT = `docs-local/versions/v0-x-perf/perf-v1.md` §一(gitignored,briefing 已内嵌全文翻译)。
- **规格**:`EventKind` 枚举 + `EventClass{Fact,LatestState,Telemetry}` 穷举分类住 progress_bridge 旁(新 kind 不写分类臂 = 编译错,AgentVendor enum-slam 同款);有状态门 = 进程全局态包在 `append_event` 内(零 call-site 改动,新写入点自动被覆盖):Fact 直通;LatestState 空/全 null 丢弃 + 语义哈希去重(排除 `ts` 等易变字段)+ per-(path,kind,scope) 最小间隔;Telemetry 不落盘只计数;未知字符串 kind = Fact 放行 + WARN + 计数;per-kind append/suppress rate+bytes 计数器(喂 V1-8/doctor);发射点 codex_app_server 全 null 先丢(保证性来自门)。零新后台定时器(全部检查发生在 append 时)。
- **DoD**:同 LatestState kind 10k 相同快照落盘 ≤1 / 全 null = 0 / 值变恰 +1;枚举穷举测试锁分类义务;`chat_turn_running_long` per-sid ≥5min 间隔;未知 kind Fact 放行测试;基线只增(1896/0 起)、clippy 0、fmt 干净。
- **验证**:2026-08-16 codex s431 交付 `11fc808`(EventKind 34 变体穷举 + reservation 门:admission 锁不覆盖 flock 写;DoD 测试全绿);规划 review 追加 `70e3b7d`(unknown kind WARN 每进程每 kind 一次,防 hook 事件族刷日志);合入后门禁见 W1 收口行。

### PERF-V1-2 Reader 统一(journal facade) W1
- **状态**:完成(5bdb1e5) · **冲突域**:`crates/ccteam-core/src + crates/ccteam-web/src(routes/api_v1.rs · routes/sessions_api.rs · queries.rs)+ 裁决追加 harness(journal.rs 新文件 · mod.rs 一行 · turns_mirror.rs)` · **建议入口**:codex 委派(规划 briefing 自包含)
- **背景**:同一 SoT 三套坏行 reader 语义(`queries.rs read_tail_events` 整文件 UTF-8 失败→假空 badge;`api_v1.rs:606` 裸 read_to_string **硬 500 今天在发生**;`progress.rs last_event` 尾行不容错);`turns_mirror last_n_turns` 假尾读(全读后切片)。正确 8KB 反向原语已存在(`progress.rs read_last_line`)未被推广。
- **规格**:core 新 journal facade 模块(泛 JSONL,progress 与 turns 共用):`last_valid` / `tail_valid(n)`(EOF 反向,I/O∝n)/ `scan_stream`(流式不物化)/ `read_delta(from_offset)`(供 W2 投影/断点);bytes 为基、坏行按条隔离、返回损坏计数;替换三家族 + turns 真反向尾读;`GET /sessions/{sid}` additive `limit`(默认 100)/`before` 分页;grep 门禁测试(web/im/core 无 facade 外 progress 直读,**先证有牙**);`fs_atomic::read_jsonl` 留 doctor/import。
- **DoD**:torn fixture 上 badge/recent-events 复活、session 详情 500→200、尾部坏行 last_valid 容错;grep 门禁红→绿留痕;基线只增、clippy 0、fmt 干净。
- **验证**:2026-08-16 codex s432 交付 `5bdb1e5`(中途正确停手申报依赖方向墙:turns_mirror 住 harness 且 core→harness;规划裁决 facade 落 `harness/execution/journal.rs`、core re-export,同 progress_bridge 模式,驳回新 crate 方案);grep 门禁红→绿实证(临时直读被抓 `progress.rs:68` 后还原);分页 additive(`limit` 默认 100/`before`/`next_before`/`has_more`);合入后门禁见 W1 收口行。

### PERF-V1-3 Runtime 多线程 + /status singleflight W1
- **状态**:完成(0bd1191) · **冲突域**:`crates/ccteam-cli/src/main.rs + crates/ccteam-web/src/routes/status.rs + core config(additive daemon.workers)` · **建议入口**:codex 委派(规划 briefing 自包含)
- **背景**:daemon 实测单线程(`Threads: 1`),同步全量读独占唯一线程 → status 进行中 `/healthz` 0.7ms→4.7s 全站冻结;全仓零 `spawn_local`,切换无结构障碍。
- **规格**:`run_start` `new_current_thread`→`new_multi_thread`(`daemon.workers` 配置默认 4,env `CCTEAM_DAEMON_WORKERS` 覆盖,workers=1 = 回退开关)+ `max_blocking_threads` 上限;`/status` 聚合 `spawn_blocking` + singleflight(并发合流一次计算,取消安全);**不碰 V1-1/V1-2 冲突域文件**。
- **DoD**:singleflight 并发合流测试 + 「请求被取消后,后续 /status 仍秒回」结构断言(v0.10.0 死锁教训);全量 `make test` 先落盘再汇总 + gateway echo 竞态族复跑;基线只增、clippy 0、fmt 干净。
- **验证**:2026-08-16 codex s433 交付 `0bd1191`(producer detached + watch channel,清 flight 与 publish 同一把 std 锁内原子,取消安全;全局聚合共享、ACL 过滤 per-caller 保持 global 语义;echo 竞态族 5 连绿;卡内全量 make test 2686/7,7 红全部 base 复现);合入后门禁见 W1 收口行。
- **W1 收口(规划,合并序 V1-1→V1-2→V1-3 全 ff)**:`cargo fmt --all -- --check` PASS;`make check` clippy 0;`make test-baseline` 并行跑两轮各撞登记 `/tmp/alpha` 族(隔离双绿、s433 已 base 对照),序列化跑 **1916/0**(基线 1896→+20);`cargo test -p ccteam-web --no-fail-fast` **401/1**(唯一红 = 登记 pty env-flake `ws_last_client_disconnect_stops_pipe_pane`);writeback 绿。

### PERF-V1-4 StatusProjection + SessionCatalog(W2;依赖 V1-1 门 + V1-2 facade)
- **状态**:完成(734eb7c) · **冲突域**:`crates/ccteam-im/src(gateway.rs + 新投影模块)+ crates/ccteam-web/src(routes/status.rs · routes/api_v1.rs · routes/sessions_api.rs · queries.rs · delegation.rs · state.rs)+ harness progress_bridge(observer 缝)` · **建议入口**:codex 委派
- **规格**:per-slug 内存聚合(last_valid、200 条 tail ring、24h 分钟桶+per-vendor、lifetime cost、per-sid cost/last-activity、委派计数、已消费 byte offset);更新点 = 门放行处单点;启动 `spawn_blocking` 水合(就绪前 stale+`warming_up`);hook 直写 fallback 靠访问时 stat+`read_delta` 补漏;SessionCatalog sid→meta 内存索引,写路径同步失效,`session_views()` 纯内存(消灭锁内逐会话磁盘读;顺手改 `sessions_api.rs:170-171` 与 `status.rs:184` 两条与事实相反的注释);消费面切换 /status(三遍合一)/projects/项目详情/session activity/budget gate/fleet_delegations;**status 聚合语义维持 global**。
- **DoD**:200MB fixture status p95<100ms 且与大小无关、每调用读量 <10MB;session list(50 live)p95<50ms 锁内零文件 I/O;基线只增。
- **验证**:2026-08-16 codex s435 交付 `734eb7c`(单一摄取路 = byte cursor+`read_delta`,`fold_event` 全仓唯一调用点在 `catch_up_locked`;observer 缝仅持久化成功后触发,OnceLock 极小;零定时器;rotation 缩小即重置守卫;两条错注释改真;status 语义保持 global;IM/MCP/web 消费面 + budget gate 全切投影;序列化 im 全量 701/0);perf 数字的正式门禁归 V1-8(本卡以「无新数据零摄取字节」结构断言证 O(1))。

### PERF-V1-5 锁窄化(W3)
- **状态**:完成(9bd876b) · **冲突域**:`crates/ccteam-web/src/routes/sessions_api.rs + crates/ccteam-im/src(gateway.rs 委派通知器/emit_delegation_progress + mcp/dispatch.rs)` · **建议入口**:codex 委派
- **规格**:两阶段模式(短锁校验+resolve+置状态带 generation → 放锁慢活 → 短锁 generation 校验提交)扫全族:`handle_session_turn` 冷 resume+submit、`handle_create_session` spawn+挤停+fsync、MCP `session_spawn`/`session_dispatch`(A2A fan-out 去串行化)、external/import `~/.claude` 扫描出锁、委派通知器仅 boundary/批量抢锁、`emit_delegation_progress` flock append 出锁;`queue_timeout` 与 `vendor_timeout` 分开计量,HTTP 入口起算整体 deadline;5xx additive `error_code`;**承接自 V1-6 的后端半件**:status/projects snapshot 带 `version` 支持 304(V1-6 重切为纯 SPA,规划决定 2026-08-16)。
- **DoD**:50 并发 turn + 冷 resume 风暴互不冻结;「5s 预算被排队吃掉」型 502 消失;generation 防 lock-gap 竞态(挤停/替换窗口旧结果必须丢弃);回归形如「重复动作 + 之后 status 仍秒回」;基线只增。
- **验证**:2026-08-17 codex s437 交付 `9bd876b`(per-live-session 单调 generation,提交前校验 6+ 处含挤停 victim 复核与委派 mirror;stale 句柄关闭并回 `session_generation_conflict`;通知器 watch-set 出全局锁,仅 boundary 进锁,append→notify 顺序保持;queue deadline 30s 起算于 HTTP/MCP 入口(env 可调),vendor 5s 预算独立;error_code 八枚 additive;/status·/projects 投影 version+ETag/If-None-Match→304,warming 期不发稳定 version;挤停既有测试零改动;新增 sessions_deadline_test + snapshot_etag_test;卡内 im 全量 704/0)。
- **W3 收口(规划,rebase+ff)**:fmt PASS;clippy 0;序列化 baseline **1924/0**(1922→+2);web 404/1(唯一红 = 登记 pty env-flake);writeback 绿。

### PERF-V1-6 前端收敛(纯 SPA;与 W2 并行,规划重切 2026-08-16)
- **状态**:完成(ee1947f) · **冲突域**:`crates/ccteam-web/web(SPA)` · **建议入口**:codex 委派
- **规格**:CostPill 与 StatusView 共用 status store(CostPill 现为可重叠定时链——修);5xx 指数退避+jitter;tab hidden 暂停;`session_lifecycle` 按 slug 增量 + 100-250ms debounce(消灭 `1+2N` 扇出);history 展开时分页(接 V1-2 limit/before);四 mount 视图共用 projects store;**后端 version/304 移交 V1-5**(域切分:本卡零 Rust 改动)。
- **DoD**:lifecycle burst 每 debounce 窗口 ≤1 次 reconcile;vitest/tsc/eslint 绿。
- **验证**:2026-08-16 codex s436 交付 `ee1947f`(25 文件全 SPA 域;ref-counted 共享 store,完成后才排下一跳 = CostPill 重叠定时链修死,env 注入式确定性测试;指数退避+jitter;hidden 暂停;lifecycle per-slug debounce;history 接 limit/before 分页;四视图共用 projects store;零 Rust 改动)。
- **W2 收口(规划,V1-4→V1-6 rebase+ff)**:fmt PASS;clippy 0;序列化 baseline **1922/0**(1916→+6);web 套件 401/1(唯一红 = 登记 pty env-flake);`make web-check` vitest **637/637**(58 文件,622→+15)+ tsc 绿;writeback 绿。

### PERF-V1-7 rotation + doctor(W4)
- **状态**:完成(a3104cf) · **冲突域**:`crates/ccteam-cli/src/doctor.rs + harness(progress_bridge rollover)+ im/progress_projection.rs(checkpoint 水合)` · **建议入口**:codex 委派
- **规格**:append 时体积检查(默认 64MB)→ 单级 rollover `<slug>.jsonl`→`<slug>.1.jsonl` + lifetime-cost checkpoint 小 json(投影水合 = checkpoint+活跃文件);doctor 增 progress 检查(体积、坏行数+offset、按字节 Top kinds、checkpoint 一致性);`--repair-progress` bytes 逐行 parse 原子重写先备份。零新定时器。
- **DoD**:rollover 触发/恢复测试;doctor 检查+修复(先备份)测试;基线只增。
- **验证**:2026-08-17 规划收口追加 `34f448f`:`load_or_recover_progress_checkpoint` 在 active/`.1`/checkpoint 全不存在时提前返回,不再物化 `.lock`(投影 catch_up 每 slug 都调,原实现让零事件项目/测试夹具凭空长出锁残留 —— 实锤 `cargo test -p ccteam-im --lib` 每跑一次在 crate 目录重生 `state/progress/*.lock` 弄脏主仓;修后同套件 CLEAN,rotation 三测试 + harness/im lib 全绿)。2026-08-17 codex s438 交付 `a3104cf`(per-slug 稳定锁串行 append/rotation/recovery/repair;64MiB 默认 + `CCTEAM_PROGRESS_ROTATE_BYTES` 覆盖;mv→流式扫 `.1` 折进累计 checkpoint(原子写,记 coverage 标记 + rotation 序号),崩溃窗 = 水合时补折未覆盖 `.1` 恰一次;cost 提取与投影共享零漂移;doctor 六项检查 + `--repair-progress` 先备份原子替换幂等;24h 桶跨界欠计为记录在案的取舍,lifetime 精确;卡内 doctor 12/12、rotation 2/2、恢复 1/1、im 705/0)。

### PERF-V1-8 观测与性能门禁(W4)
- **状态**:完成(d3931fb) · **冲突域**:`Makefile(perf-gate)+ web 层指标(middleware)+ im/gateway 锁计量 + 生成式 fixture 测试` · **建议入口**:codex 委派 + 规划收口(verify 登记归规划)
- **规格**:per-route latency(>500ms WARN)、progress bytes read/records parsed/invalid lines、per-kind append rate/bytes、gateway lock wait/hold、blocking pool queue;测试时生成 fixture(~150-200MB/100 万行,中部 torn UTF-8+尾部坏行;50 live/380 stopped;单会话 1 万 turns),挂 env 开关/`make perf-gate`,普通 CI 不变慢;断言 §〇 数字(status p95<100ms 与大小无关、healthz p99<10ms、registry 锁持有 p99<5ms)。
- **DoD**:perf-gate 目标数字全绿;`.loop/verify/README.md` 登记(规划执笔);基线只增。
- **验证**:2026-08-17 codex s439 交付 `d3931fb`(与 V1-7 的 journal.rs 语义冲突由 s439 自解:保 `DetailedScanSummary` 形状,metrics 直接消费 next_offset/valid_count/corrupt_count;指标四面 = 路由延迟 middleware(>500ms WARN)/ journal 读累计 / top kinds / gateway 锁 wait+hold(>250ms WARN),tokio 阻塞池指标因 stable API 缺席跳过留痕;fixture 生成器 176.8MiB/100 万行/0.16s 确定性)。**规划在最终合并树复跑 `make perf-gate` 全绿**:status p95 25.43ms(目标<100)· 投影摄取 0B · 读放大 0.008MiB(目标<10)· health-during-status p99 0.14ms(目标<10)· session-list p95 42.32ms(目标<50)· tail_valid(200) 1.07ms(目标<50)· 10k-history 1.47ms(目标<100)· 锁持有 p99 0.176ms(目标<5)。
- **W4 收口(规划,V1-7→V1-8 rebase+ff)**:fmt PASS;clippy 0;序列化 baseline **1924/0**(W4 新测试全在 tests/*.rs 集成口径);web 406/1(唯一红 = 登记 pty env-flake);全量 make test 133 bins **2717/7**(7 红全为登记族:`/tmp/alpha` ×2(新面孔 `gateway_status_shows_real_vendor_resume_uuid` 实锤同用字面 `/tmp/alpha`,隔离绿)+ CLI 子进程计时 ×1 + DSH default-model ×4);writeback 绿。

### DSHCFG-1 DSH 配置单一解析器 + 租户家种子/跟随 + de-scrub(v0.10.1 主卡)
- **状态**:完成(68c4bbd) · **冲突域**:`crates/ccteam-harness(dsh_acp) + crates/ccteam-web(dsh_web) + crates/ccteam-im(SpawnCtx owner 装填)` · **建议入口**:codex 委派(规划 briefing 自包含)
- **背景**:owner 实测「受管 DSH 会话开箱即用、DSH web 空间却空配置」= 两条 spawn 路径各自决定凭据的病根;PRD = `docs-local/versions/v0-10-1/prd.md`(已拍板:D26 turnkey 取代 D22 / D27 不做开关 / D28 refresh-if-unmodified)。
- **规格**:PRD §三 —— `dsh_acp` 内单一解析器 `dsh_config_source(owner_tag)`(env → operator 家 → 租户 web 家(有凭据才算,二件套同源)→ 回落 operator → 全无拒绝);`SpawnCtx.owner` additive(meta `owner` 现成);`build_spawn_spec` 换源到解析器;web home 路径派生收敛进 harness(ccteam-web 删本地副本);租户家物化时种子二件套 + `.ccteam-dsh-seed.json`(sha256 标记,零解析)+ 实例启动 refresh-if-unmodified(无标记且文件存在 = 视为已修改,永不碾压);`build_web_spawn_spec` 撤 `scrub_provider_env` 整个字段(pre-v1.0 无 shim)。
- **DoD**:解析器五臂穷举测试 · 种子/刷新/不碾压/混源必败/受管按 owner 取源(byte-compare)定向测试 · 翻转 `web_profile_factory_has_no_mirrored_operator_credentials` 为种子断言 · `make test-baseline` 只增(基线 1895/0)· clippy 0 · fmt 干净 · writeback 绿。
- **验证**:2026-08-16 `cargo fmt --all -- --check` PASS;`make clippy` PASS(0 warnings);`make test-baseline` PASS = 1896/0(+1);`cargo test -p ccteam-harness --test dsh_acp_test` 新增 DSHCFG 定向用例全 PASS(同 target 既有 4 个 default-model env 红保持登记态)。

### DSHCFG-DOCS v0.10.1 用户面文档
- **状态**:完成(15c0398) · **冲突域**:`README.md + docs/` · **建议入口**:codex 委派(同会话收尾)
- **规格**:usage(EN/CN)DSH Web 节改写为当前能力:「出厂即用(跟随本机 DSH 登录);在 DSH Settings→Models 配自己的 key 后,该身份所有 DSH 会话(含 ccteam 里雇的)都用你的」+ PRD §五诚实三条融入;README 对应行同步。
- **DoD**:docs 最低门(fmt + writeback);不写版本进展措辞。
- **验证**:2026-08-16 `cargo fmt --all -- --check` PASS;`rg "ships with no|出厂不复制|v0\\.10\\.1|new in this version" README.md docs/usage.md docs/usage-cn.md` 无用户面陈旧/版本进展命中;`.loop/verify/writeback.sh` 收口执行。

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
