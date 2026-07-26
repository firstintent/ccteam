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

### IM-HINT-1 IM 命令回执「下一步」提示补全:/model + picker 按钮一致性(owner 直驱 2026-07-26)
- **状态**:完成(0dc6fab) · **冲突域**:`crates/ccteam-im(gateway)` · **建议入口**:codex 委派(规划发卡 + review)
- **背景**:owner 三点(项目切换尾加 /sessions、会话切换尾加 /status、模型切换尾加 /status)。核对现状:typed `/cd`→`↓ 本项目会话 → /sessions` 与 `/use`→`↓ 查看状态 → /status` **repo 已实现**(`command_next_hint` + handle 路径 choke,owner live 二进制旧未见);真缺口 = ① `/model` 是 vendor 透传(五 adapter 生成同步回执「已切换 model → …」,不经 gateway 命令 choke,注释明示 never reach);② picker 按钮路径(`resolve_nav_selection` nav:cd/nav:use)返回裸串,与 typed 同动作不同待遇。
- **规格**:A. `/model` IM 尾行:在 IM handle 路径对 vendor 透传返回的同步 Directive replies,若原文首 token == `/model` 且 channel != "web" 且 replies 非空 → 最后一条 reply 追加 `↓ 查看状态 → /status`(harness 五处回执生成点零碰,web SSE/receipt 语义不变;terminal 无同步回执自然豁免);B. picker 一致性:nav:cd → `↓ 本项目会话 → /sessions`、nav:use → `↓ 查看状态 → /status`(同 web 豁免;与 typed 共用 hint 常量,禁字面量复制)。**明确不做**:/effort 等其他透传(owner 未点名)、web 面(GUI 导航既有豁免)、`/compact /clear` 透传。
- **DoD**:定向测试先红后绿(IM `/model` 回执带尾行 / web 无尾行 / nav:cd·nav:use tap 带尾行);`make check` clippy 0;`make test-baseline` 红不增(HERM-1 口径)、数目净变对账;fmt 干净;writeback 绿;两 commit(实现→写回,sha 精确)。
- **验证**:实现提交 `0dc6fab`。有牙 red→green:仅翻测试后旧源上 IM `/model` 回执缺 `/status` 尾行红、nav:cd 完整回执缺 `/sessions` 尾行红;web `/model` 字节一致守卫在旧源即绿;实现后 IM/web `/model` + nav:cd/nav:use + 既有 typed `/cd`/`/use` 定向测试全绿。门禁:`cargo fmt --all -- --check` 绿;`make check` clippy 0 warnings;`make test-baseline` 1666 绿 + 1 个已登记 HERM-1 红(`web_chat_bridge…survives_restart`),对照上卡 1664 绿 + 同一红净 +2,恰为新增两只 `/model` 测试;nav 测试仅收紧断言,测试数净 0;无新增红。实现 diff 仅 `crates/ccteam-im/src/gateway.rs`,harness/web 零碰;两条 hint 生产字面量各自仅在共享常量单点定义;`writeback.sh` 绿。

### WEB-IA-1 web 信息架构改版:market 迁 flow + ops 会话大表删除 + 「接入」聚合(owner 直驱 2026-07-26)
- **状态**:完成(f7f5d67) · **冲突域**:`crates/ccteam-web/web` · **建议入口**:codex 委派(规划发卡 + review)
- **背景**:owner 三项 UI 直驱:① 插件市场从设置迁到工作流下,市场内 skills 优先(全局 skill 库落地后按项目筛选不友好);② 设置·运维总览的会话 fleet 大表(`status-sessions`,「N live · M idle」)占版面且拓扑已有 团队 视图,删;③ token/授权四散面(用户登录链接 / 外部 MCP 配置 / 卫星加入 / IM 凭据)聚合一处 —— 规划钉设计 = 新设置 tab「接入 Access」(admin-only)四卡分区;外部 MCP 配置 JSON 此前无任何页面渲染(只在文档),本卡首次给它安家。纯 UI 层,REST/后端零碰;ACL 语义不变(后端 403 兜底,UI fail-closed)。
- **规格**:A. market 迁移:SettingsView ITEMS/SettingsTab 去 market(非 admin 默认 tab market→general),WorkflowView TABS 增 market(序 skills/roles/market/mcp/evolution),App.tsx `/settings/market`+`/marketplace` → `/flow/market`,ChatConsole onOpenMarket 改指;B. 市场内:默认 category agent→skill 且 skill 排首,project 选择器仅 agent/plugin 类显示(skill 装全局库,无项目语义);C. StatusView 删 `status-sessions` fleet 大表区块(stat-grid 会话小瓷片保留;rail prop 失依随删);D. 新「接入 Access」tab(admin-only fail-closed,位 ops 后):①外部 Agent MCP 卡 = mcpServers JSON 模板(origin+`/mcp`+Bearer=当前登录 token,client-side 渲染零新端点)+ 复制;②卫星节点卡 = HostsView JoinCard 整体迁入(hosts 面留列表 + 指路链接);③IM 凭据卡 = admin 全局 Telegram/Lark 区块自 admin tab 迁入(tenant「我的 IM bot」留账号 tab);④用户登录链接卡 = tenant handle + 复制链接(复用 `GET /users/{id}/link`;用户生命周期管理留 管理员 tab);i18n zh/en 双语新键。
- **DoD**:`make web-check` 绿(tsc/eslint/vitest,计数净变逐只对账);既有五处测试更新 + Access 定向测试(admin 门 fail-closed + 四卡渲染);rust 零碰;writeback 绿。
- **验证**:`make web-check` 绿(eslint + vitest **422/422**);`npm run build` 绿(tsc + vite)。vitest 对发卡锚 422 净 0:删除 Status fleet 专属 7 测试,新增 Access 4 + Marketplace 2 + Workflow market 1;其余均为既有断言随 IA 更新。`git diff --check` 绿;实现提交 18 文件全部位于 `crates/ccteam-web/web/`,`.rs` 零命中;`writeback.sh` 绿。

### MCP-CULL-3 session_spawn `protocol` 参数删除(owner 直驱 2026-07-26;wire 契约变更)
- **状态**:完成(9c2a89e) · **冲突域**:`crates/ccteam-im/src/mcp + crates/ccteam-cli(mcp_session_tools 测试) + docs/orchestration` · **建议入口**:codex 委派(规划发卡 + review)
- **背景**:外部 agent 反馈「grok 必须 acp 只藏在描述括号里,第一次即踩坑」。MCP-DX-2(1ab85da)已把静默覆盖修成派生+冲突可操作错误但为守 wire 形状保留了字段;owner 本日追加拍板:**字段整个删掉** —— 每个 vendor 只有一种最佳协议(claude/codex=stream-json,grok/opencode/kimi=acp),调用方零选择;terminal/tmux 面后续另案退役。先例 = host 参数删除(`HOST_SPAWN_PARAM_REMOVED`,v0.9.2:schema 不列 + 传入即硬错)。
- **规格**:A. schema 删 `protocol` property(protocol.rs session_spawn inputSchema);B. dispatch 镜像 host 门:`args.get("protocol").is_some()` → 稳定可操作错误(错误文案含 vendor→channel 派生表 + omit 指引;terminal 不再特判,任意值同一错误);`resolve_session_protocol` 简化为纯派生 `derive_session_protocol(vendor)`;spawn 响应/list 的 `protocol` 输出字段保留(观测面);C. 测试翻转:protocol.rs facet 测试 + cli mcp_session_tools schema 测试(protocol 出 presence 列表 + 加 absence 断言,镜像 host)+ dispatch 派生/removal 测试重写(五 vendor 派生 + 任意显式值(含此前被接受的一致值)同错);D. docs:orchestration.md(+cn)facet 列表去 `protocol?`、措辞改「无 protocol 参数,通道由 vendor 派生,传入即硬错(同 host)」——cn 版仍是 DX-2 前旧文(「强制 protocol:"acp"」括号即反馈踩坑点)一并修。**边界零碰**:REST CreateSessionForm/web SPA 协议选择(admin beta,terminal/rmux 退役另案)、内部 SessionProtocol/adapter/session_meta、IM/CLI 命令面。
- **DoD**:翻转测试对旧代码红、新代码绿(留痕);`make check` clippy 0;`make test-baseline` 红不增(本机 HERM-1 三红口径)、数目净变逐只对账;fmt 干净;writeback 绿。
- **验证**:有牙 red→green:仅翻测试后旧源上 CLI schema 测试明确红(`protocol` 仍存在),IM 定向测试因 `derive_session_protocol`/`PROTOCOL_SPAWN_PARAM_REMOVED` 尚不存在编译红;实现后 CLI schema + IM schema + 五 vendor 派生/显式值拒绝定向测试全绿(含匹配 acp、冲突值、terminal、bogus、null 同一稳定错误)。门禁:`cargo fmt --all -- --check` 绿;`make check` clippy 0 warnings;`make test-baseline` 1664 绿 + 1 个 HERM-1 已登记红(`web_chat_bridge…survives_restart`),对照锚 1662 绿 + 同族 3 红总数同为 1665——本卡重写 1 测试为 1 测试,净测试数 0,另两只宿主泄漏红本轮转绿,无新增红;`writeback.sh` 绿。残留:`resolve_session_protocol` 0 命中;指定 doctor/e2e/subprefix/web 测试无 spawn 调用传 `protocol`;`"protocol"` 仅余拒绝门/缺席断言/拒绝门测试与 spawn 响应观测字段。

### MCP-CULL-1 删除 MCP `screenshot` 工具(owner 直驱 2026-07-26;工具面 8→7)
- **状态**:完成(1ab85da) · **冲突域**:`crates/ccteam-im/src/mcp + crates/ccteam-cli + crates/ccteam-web(tests) + docs + AGENTS.md` · **建议入口**:规划(控制)会话(涉契约 + 治理面)
- **验证**:`doctor --verify-mcp` 定向测试 = 7 工具 0 STUB(json + human 两版式);protocol/dispatch/groups/mcp_serve 四处定义与执行路径全删,screenshot 调用 = `unknown tool` 定向测试(im/cli/web 三层);`CCTEAM_DISABLE_TOOLS=screenshot` 陈旧 token 走 unknown-token 忽略路径(定向测试);web `/screenshot` 路由、core `render_screenshot`、IM `/screen` 零碰(diff 复核);web tenant 测例改用 `status{project}` 保住 User-vs-Admin 路由边界断言。门禁:`make check` clippy 0;`make test-baseline` 1672 绿 + 3 红 = HERM-1 登记同三只(文件零碰),对照 MCP-DX-1 锚净 +2 与新增测试数吻合;fmt 干净;writeback 绿。文档:AGENTS §〇×2/§三(红线行 screenshot 提法改 web 端点,owner cull 令的后果性措辞)/§四 + README + usage(+cn) + orchestration(+cn) + tech-design MCP census(15 时代陈表顺带重写为现势 7 工具,TD-SYNC-1 范围其余不动)。
- **背景**:owner 指令「删除 mcp__ccteam__screenshot,这个是 tmux 时代的遗留产物」= MCP wire 契约变更签核(登记 state.md)。范围仅 MCP 工具面:web REST `/screenshot/<slug>.png`、core `render_screenshot`、IM `/screen` 命令 = terminal 协议维护面,owner 未点名,不动。`chat_send_file` 同问评估结论 = **保留**(真 IM 外发通道:Telegram/Lark 用户无法打开 daemon 本地路径,file 送达与文本回复同 funnel,与 tmux 无关;两条 caller 路径 sid-era 均活);其描述陈旧文案(tmux 共享盘/screenshot 组合/V0.8.4 考古)随卡刷新。
- **规格**:protocol.rs 定义+本地执行+import、dispatch.rs `is_screenshot_call`/`execute_user_screenshot`、groups.rs `Screenshot` 组(未知 token 既有忽略语义兜底旧 `CCTEAM_DISABLE_TOOLS=screenshot`)、cli mcp_serve 期望集/内嵌测试、doctor_verify_mcp/mcp_e2e/mcp_subprefix/mcp_disable_groups、web mcp_tenant_bearer_test(screenshot 拒绝测例改 cull 回归守卫);文档数字与清单同步(§五.7 家规)。
- **DoD**:`doctor --verify-mcp` = 7 工具 0 STUB;`make check` clippy 0;基线无新红(删除面净减测试在案);writeback 绿。

### MCP-BEACON-1 status 裸名发现别名 + spawn 配方(owner 拍板 2026-07-26「纯别名,方便后续随时改;opencode 排最后」)
- **状态**:完成(ffcf817) · **冲突域**:`crates/ccteam-im/src/mcp + crates/ccteam-cli + crates/ccteam-web(tests/SPA) + docs` · **建议入口**:规划(控制)会话(契约变更签核 = owner 本条)
- **验证**:派生测试(AgentVendor::ALL 全员恰一次 + opencode 殿后 + `mcp__ccteam__` 前缀下 64 字符上限)+ 等价测试(alias 与 status tools/call 响应逐字节一致 + schema 一致)+ 配方单测(只列已装 vendor / 全未装 = 空串)全绿;三路同判(protocol call_tool / dispatch is_status_call / cli forward_status)入库;doctor --verify-mcp = 8 工具 0 STUB(admin 组 2);`make test-baseline` 1662 绿(+2)+ HERM-1 同三红;clippy 0;vitest 422;fmt 干净;writeback 绿。计数/文档八处同步在案(AGENTS/tech-design/README/usage±cn/orchestration±cn/SPA)。
- **背景**:WorkBuddy P0-1 诊断(裸名宿主抹掉描述与 instructions,工具名是唯一发现面,9 个名字里没有 vendor 关键词 → 「用 grok 搜索」第一轮发现失败)。规划初判改名 status,owner 中途改令**纯别名**(alias 可随时改/删,status 零 churn);vendor 序 = 枚举序但 **opencode 殿后**(owner 钦点)。名字含 "agents" 的变体被放弃 —— 64 字符工具名上限扣除 `mcp__ccteam__` 前缀后要给第六个 vendor 留余量。
- **规格**:新增 `claude_codex_grok_kimi_opencode_status`(7→8)= status 纯别名(protocol.rs 定义 + call_tool / dispatch is_status_call / cli forward_status 三路同判;admin 组);名字派生测试锁死(AgentVendor::ALL 全员恰一次 + opencode 殿后 + 64 上限);等价测试(响应逐字节一致);status 响应(vendor panel 路径)加 `recipes` 块 = 已装 vendor 各一行 session_spawn 一行式 + collect/dispatch 收尾行(纯静态拼接零 LLM,措辞不超 spawn 描述既有定性);全量计数/文档同步(AGENTS §〇×2/§四、tech-design census、README、usage±cn、orchestration±cn、SPA WorkflowView)。
- **DoD**:定向测试绿(派生/等价/配方);doctor --verify-mcp = 8 工具 0 STUB;基线只增;clippy 0;web-check 绿;writeback 绿。

### MCP-CULL-2 screenshot 关联面全删(owner 扩令 2026-07-26「screenshot 关联的一并删」)
- **状态**:完成(614ed9b) · **冲突域**:`crates/ccteam-core(screenshot/paths/deps) + crates/ccteam-web(routes/tests/SPA 文案) + crates/ccteam-im(gateway /screen) + crates/ccteam-harness(注释) + docs` · **建议入口**:规划(控制)会话(契约扩令)
- **验证**:-1376 行 / 4 文件删除;core 四依赖(vt100/image/imageproc/ab_glyph)出 Cargo.toml(harness 自有 vt100 保留,web 终端/快照用);漏网两处数字断言随卡补(`mcp_http_test` 8→7 —— 纯 count 断言首轮字符串 grep 抓不到,经验:契约数字变更须按数字扫非仅按名;SPA WorkflowView「8 tools…screenshot」文案)。门禁:clippy 0;fmt 干净;`make test-baseline` 1660 绿 + HERM-1 同三红,-12 恰为删除的 screenshot 模块内嵌测试(8+4 逐只对账,owner 令下的功能退役非回归);`make test-web` 除已登记 pty_ws env-flake 族 1 只外全绿;vitest 422 / tsc 干净;残留 grep 全为防复活反断言;writeback 绿。
- **背景**:MCP-CULL-1 收口报告列明保留面(web `/screenshot` 路由 / core `render_screenshot` / IM `/screen`),owner 明示一并删。SPA WorkflowView 「8 tools …/screenshot/…」文案系首轮漏网,随卡清。
- **规格**:删 core `screenshot` 模块 + `paths` 两 helper + vt100/image/imageproc/ab_glyph 四依赖(harness 自有 vt100 为 web 终端/快照用途,不动);删 web `/screenshot` 路由 + `screenshot_test.rs` + auth/state/pane_snapshot 注释残留;删 IM `/screen` 命令(spec + handler);harness 注释去 screenshot 提法(capture 函数被 web 终端/快照使用,保留);docs(AGENTS 红线行措辞 / tech-design / usage±cn `/screen` 行)同步。基线数字随删测试净减 —— 逐只列账,非回归。
- **DoD**:workspace 零 screenshot 符号残留(注释与「用户报错截图」语义除外);`make check` clippy 0;`make test-baseline` 红不增、减少数与删除测试清单吻合;`make web-check` 绿(SPA 文案);writeback 绿。

### MCP-DX-2 外部反馈第二轮:关键词可搜性 + protocol 诚实校验 + 单项目默认(owner 直驱 2026-07-26)
- **状态**:完成(1ab85da) · **冲突域**:`crates/ccteam-im/src/mcp`(与 MCP-CULL-1 同域,同会话串行) · **建议入口**:规划(控制)会话
- **验证**:有牙实锤 = 四处缺陷态突变(protocol 静默覆盖复原 / admin·tenant 默认各拆线 / spawn 描述反引号回归)恰咬红对应 4 只新/强化测试,复原全绿(112→116)。A:spawn 描述 vendor 名纯文本 + 反断言(任何 `` `vendor` `` 形态不得回潮);instructions 补单项目默认语义。B:`resolve_session_protocol` 单元测试覆盖五 vendor 派生/一致接受/冲突错误(错误文案含「omit `protocol`」恢复指引)/terminal 拒绝/typo 报错;schema 参数保留(wire 形状不变),描述改「OMIT——derived from vendor」。C:admin 恰一注册项目自动默认(fixture gateway 无 config watcher → 确定性 `unknown project: robchat` 证默认已命中)+ tenant 恰一可见项目走全 execute 路径落 `project:"alice"`;既有 missing-project 错误测试改双项目 fixture,byte-identical 防枚举断言保持。门禁同 MCP-CULL-1(同 commit)。
- **背景**:QoderWork + WorkBuddy 第二轮调用复盘(owner 转交 2026-07-26)。代码核对实锤三缺口:① spawn 主描述 vendor 名全反引号包裹 → 宿主 ToolSearch 分词不命中(同宿主实测 status 纯文本正常命中 = 对照实证);② `protocol` 参数对 grok/opencode/kimi **静默覆盖**成 acp(QoderWork「静默失败」踩坑;adapter.rs 实锤 Claude 无 ACP 臂、codex 值仅 informational → 参数零信息量);③ 恰一注册项目时 missing `project` 仍硬报错(WorkBuddy P1;外部宿主 cwd 不在项目内必撞)。
- **规格**:A. spawn 主描述 vendor 关键词纯文本化(status 已是),测试加反断言(描述不得含反引号包裹的 vendor 名);B. protocol 静默覆盖 → 派生 + 校验:省略 = vendor 派生(claude/codex=stream-json,grok/opencode/kimi=acp),显式一致 = 接受,显式冲突 = 可操作错误(schema 参数保留,wire 形状不变;terminal 拒绝语义不动);C. admin/tenant 恰一(可见)项目自动默认进该项目 + `project` 参数描述加发现提示(status 列 slug);既有 missing-project 错误路径测试改双项目 fixture(语义翻转),新增单项目默认 + protocol 冲突定向测试。
- **明确不做**(沿 MCP-DX-1 决议 + 本轮裁定):新工具 grok_search / beacon status / session_spawn_advanced / project_list(钢线「改进 ≠ 加法」;spawn+task+wait_seconds 已是一次调用拿结果,QoderWork 自己也确认好使);工具改名嵌 vendor 关键词(wire 契约变更,未授权,且 WorkBuddy 宿主抹描述属宿主缺陷);collect 结构化/turn 过滤(家 = P1-2 + A2A-OBS-1);grok 完成账本缺 tokens/cost(vendor usage 不上报,家 = A2A-OBS-4「usage 诚实外显」+ OBS-5E 捕获核查);描述整体再瘦身(MCP-DX-1 已 -792 字符,本轮只做关键词修正不重排)。
- **DoD**:定向测试先红后绿留痕;`make check` clippy 0;基线只增;writeback 绿。

### GOV-CE-3 §三红线增删(owner 签核 2026-07-26「批A B C」+「E删除」)
- **状态**:完成(23d0cef) · **冲突域**:`AGENTS.md + docs/dev/tech-design.md + .loop/` · **建议入口**:规划(控制)会话(人工门:红线)
- **验证**:行级 diff 恰为 +3/-1 —— 新增「引擎零 LLM」「daemon 无自主内容决策循环」(紧跟 No prompt injection,引擎中立三连)+「vendor 配置足迹 = 只写自家 MCP 注册」(挨「不改写 CLAUDE.md」写入边界组);删 README 行 = **迁家非删规则**(§五.7 措辞补全为唯一家,tech-design R11 注记);AGENTS.md 146→148 行 / 27.0K→27.7K;最低门绿(fmt-check + writeback)。
- **规格**:字面 = owner 已批 chat 提案(A/B/C/E);**D(never-execute 并入 #15)未批不动**。A = 引擎不调用任何 LLM,推理只在 agent session 内(自 tech-design web 章升格);B = vendor 全局配置唯一写 = 幂等注册自家 MCP 条目(DX-DOCTOR-1 偏差裁决引用过的事实红线成文);C = daemon 只响应消息/排程/连接、不产生任务不选择工作(自 §〇 叙述升格)。

### GOV-CE-2 §三红线表述治理(owner 签核 2026-07-26)
- **状态**:完成(f0c834c) · **冲突域**:`AGENTS.md + docs/dev/tech-design.md + .loop/` · **建议入口**:规划(控制)会话(人工门:红线)
- **验证**:变更行恰为已批 12 行,**「不动」7 行零字节未入 diff**(filtered diff 复核);`会话 = resume-by-session-id` 行并入 `session = 独立一等实体`(术语保留可 grep);§三 12,524→9,137 字符(-27%),AGENTS.md 147→146 行 / 30.4K→27.0K(自 GOV-CE-1 前累计 -41%);最低门绿(fmt-check + writeback)。
- **规格**:语义零增删 —— 删三类(实现考古/版本叙事/与 §四·工具自描述重复),每格留不变式 + choke point 名 + 诚实范围 + 反转护栏;行标题去 v 标签 3 处 + ACL 副题「档0/档1」→「IM 面/web 面」。底稿 = owner 已批 chat 提案(2026-07-26,job 暂存 redlines-proposal.md)。
- **偏差**:提案原文「tech-design §0 修为按行名引用」执行收窄为最小同步(R10 陈旧路径修正 + R4 并行注记 + 重复「已退役」段改指针)—— body 十余处 R-code 引用,全量转行名会造成孤儿引用,归 TD-SYNC-1 一次做完。

### TD-SYNC-1 tech-design 全文陈旧校对(GOV-CE-2 顺带发现)
- **状态**:待排 · **冲突域**:`docs/dev/tech-design.md` · **建议入口**:规划(控制)会话(docs 治理面)
- **背景**:GOV-CE-2 排查实锤 §0 R-code 速查漂移(R1「文件系统是状态面」/R9「crate 拓扑」不在现行 §三;R10 旧 `<team>-<slug>` 路径已随卡修正)+ 正文残留 v0.9.0 前状态(§6.x 仍写「`ccteam init` 种默认 `cto.md`」)。
- **规格**:全文一轮校对 —— R-code legend 与 body 引用对齐现行 §三(或整体改行名引用)、清 pre-v0.9.0 叙述(cto 种子/team 路径/退役命令)、协议细节改代码指针;语义争议处停手报规划。
- **DoD**:grep「种默认 cto」「<team>-<slug>」= 0 命中;R-code 引用无孤儿;最低门绿;writeback 绿。

### GOV-CE-1 AGENTS.md 上下文工程瘦身(owner 直驱 2026-07-26)
- **状态**:完成(ab275be) · **冲突域**:`AGENTS.md + .loop/` · **建议入口**:规划(控制)会话(治理面)
- **验证**:最低门绿(fmt-check + writeback);AGENTS.md **178→147 行 / 45.7KB→30.4KB(字符 -33%)**;§三红线表 + vendor 红线 + §五纪律 + §六坑**逐字未动**(diff 逐行复核;§三唯一笔误 `(project,role)` 已还原);CLAUDE.md 软链完好;仓内无 §七 悬空引用(代码注释 `PRD §七` 指 v0.8.11 PRD,无关)。
- **背景**:owner 指令按 @trq212 帖治理本仓;grok s116 抓帖核实 = Anthropic《The new rules of context engineering for Claude 5 generation models》(2026-07-24,Claude Code 对 Opus 5 / Fable 5 删 ~80% 系统提示词、coding 评测无损;Then→Now 六法则:规则→判断 / 示例→接口 / 前置堆料→渐进披露 / 重复→工具自描述 / CLAUDE.md 记忆→auto-memory / 瘦 spec→富引用)。
- **规格**:删 = §〇 对 §三/§四 的复述、协议/API/路由枚举(家 = 代码 + tech-design 指针表)、版本考古叙事(家 = `.loop/history.md` + docs-local)、§七(并入 §六 fmt 条)、退役命令 necrology(clap `--help` 即自描述接口);留 = 红线 / 纪律 / 实战坑 / 文档地图(帖子的 KEEP 类:契约 + 治理 + 项目私有事实)。红线与流程语义零变更;「已退役概念」脚注去重为 §〇 尾注一行。

### DX-DOCTOR-1 doctor 体检面重排 + daemon 启动自动注册五 vendor MCP(owner 直驱 2026-07-25)
- **状态**:完成(4b1fc4d) · **冲突域**:`crates/ccteam-cli + crates/ccteam-core(host_registry/mcp_register) + crates/ccteam-web(routes/hosts)` · **建议入口**:codex 委派(cct-codex)+ grok 对抗 review
- **验证**:codex s113 三轮(实现 + F1-F3 + R1-R4)+ grok s114 侦察与对抗 review(verdict FIX,4 发现全采纳:probe 不查退出码 / override 判据过松会误建 vendor 配置 / hint 门 healthy 向无测试 / SPA fixture 只编码翻转前形态);fmt + clippy 0 warnings;doctor 契约测试重写 + autoreg 密封测试(HOME/CCTEAM_HOME/五 vendor env/PATH 全 pin,断言不逃逸沙箱)+ 纯渲染单测(daemon hint 双向 + 静默 pass 计数);真机目验 daemon up/down 两态输出。基线:本机 = HERM-1 三红 + `resume_*` 族瞬时红(gateway_resumes_dead_session_on_next_turn,3/3 复跑绿,本卡 diff 零碰 ccteam-im)之外全绿;vitest 422 / tsc 干净;干净环境仲裁待 dev→main PR CI(本机无 gh,开 PR 在 owner)。
- **偏差**:① openapi 路由清单测试在 origin/dev 本来就红(schedule 三路由入库时无 PR/CI),随卡修复并在 commit 注明;② `make web-check` 被 ChatComposer.tsx 两个存量 eslint 错误挡住(文件不在本卡 diff)= origin/dev 既有,已另起 chore 修复(独立提交);③ auto-register 使「注册写入」的触发时机从"仅显式命令"扩到 daemon start —— vendor 足迹唯一写 = 自家 MCP 注册的红线语义不变,owner 本口头指令即授权。
- **背景**:owner 拿远端 `ccteam doctor` 输出反馈:排版散(每 vendor binary/auth/MCP 三行)、tmux/legacy-service/exe 是与用户无关的噪音、daemon down 提示埋在中段;MCP 注册目前要用户在 web 主机页手点。另发现 `AGENT_PROBE_SPECS` grok/opencode `mcp_registrable:false` 是 v0.8.18 陈旧值(v0.9.3 已对称注册五 vendor),doctor 只查 claude/codex/kimi 三家 MCP。
- **规格**:A. doctor 输出重排 = agents(五 vendor 每家一行,折叠 binary+auth+MCP)/ ccteam(daemon/version/pricing/home/host-skew)/ projects(skills 行仅 WARN 时现身)三段;删 tmux 检查;legacy-service 仅检出时现身;updates 行拆成 version 行(去 exe 噪音)+ 每个 skew 卫星一行;daemon down = 短行 + **汇总后末行显著提示** `ccteam daemon start`;TTY 上色(尊重 NO_COLOR);claude MCP 未注册由 FAIL 降 WARN(理由 = B 起动自愈)。B. `ccteam start`(run_start)启动时 best-effort 幂等注册五 vendor 全局 MCP(binary 可解析或 config 足迹已存在才写;失败 warn 不阻断;吞并 heal_codex_mcp_if_stale)。C. 陈旧 flag 修正:AGENT_PROBE_SPECS grok/opencode registrable=true + hosts.rs `mcp_registered()` 接 grok/opencode 真实检查 + register-mcp 端点文案随 spec 列表。红线不破:doctor 保持只读;vendor 足迹唯一写 = 自家 MCP 注册(仅触发时机扩至 daemon start)。
- **DoD**:doctor 新版式定向测试更新(daemon-down 末行提示、tmux 不再出现、claude MCP 未注册 = WARN 且 exit 0);auto-register 沙箱测试(严禁写真实 HOME,pin HOME+CCTEAM_HOME);`make test-baseline` 只增(同机对照 origin/dev);clippy 0 warnings;fmt 干净;writeback 绿。

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
