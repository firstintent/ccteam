# ccteam 云端托管 code-agent 控制中心 —— 赛道需求探索

> 2026-06-10。输入:LAP(litellm-agent-platform,HEAD 05bc97c)全仓侦察、vendor 托管轨迹、ccteam v0.8.10 位置图、公开格局与买家研究、46-agent 红队备忘;31 条裁决收敛为 26 个候选位(20 keep + 6 CUT),本文为收敛后的需求 SoT。关键代码锚点(lib.rs 默认 bind、mcp_session_tools.rs 无硬边界自认、gateway.rs 零 budget 等)已复核属实。

## 1. 赛道定义与买家

赛道 = 坐在 code-agent runtime/CLI 之上的**自托管控制面**:统一创建、驱动、观测、治理多个 agent 会话。LAP readme 给出教科书式定义:跨 runtime 统一 API、Access(免 vendor console)、持久 session、CRON、memory。

买家四分(buyer-jobs):**① 公司平台组**(SSO/审计/策略)——正被 GitHub Agent HQ、Claude Enterprise admin(三级 spend limits + SSO)原生收编;ccteam 单 uid 无硬边界,结构性错配,弃。**② 小团队/微型 agency**——主买家:JTBD =「一台云主机 = 一支随叫随到的交付队」,三痛 = 自托管安全运维(OpenClaw 3万–13.5万公网暴露实例、CVE-2026-25253 zero-click、infostealer 盗刷,Bitsight)、多客户项目隔离与寻址、按项目记账+预算(锚:Devin 重度使用 ≈ 额外 $3,600/月);已证实付费形态 = 为「装好且不被打」掏钱(Hostinger/Contabo 一键 VPS、RunMyClaw managed、OpenHosst $2.99/月),非买 license。**③ 个人 power user**——与 requirements.md「一人驱动上百 agent」字面重合,形态最贴,但付费天花板 $0–9/月(Omnara $9、Happy 免费、Bloop 关停=变现难实锤),定位 adoption 漏斗。**④ SaaS 嵌 fleet**——买 E2B/Daytona + SDK 原语,与 tmux+单 uid+IM 形态相反,弃。**主押②,③引流,①④显式不追。**

## 2. LAP 已立的 table-stakes 与 vendor 自营托管趋势

**LAP 立起的入场线**(全仓证据):跨 runtime 统一 API(RuntimeAdapter 6 方法、Anthropic-shaped SDK、alias 即插即用);provider key 托管(加密入 DB、「调用者永不见 provider key」,docs/auth.md);持久 session(SessionRow + SSE runtime_events + interrupt/abort);spend 记账(SpendLogs,只记账无强制);CRON routines(表+trigger 端点,无 tick);HITL inbox;per-agent memory KV;MCP 透传+registry;17 页 dashboard。LAP 自暴的反面=赛道普遍翻车点:master key 缺省全放行、gateway key 内存态且与 master 等权、runs 内存态、沙箱一次性 run、全仓无 PTY/tmux、IM 仅 per-agent Slack、Teams 页「not configured in this build yet」、skills/rules 走 compose_agent_system_prompt 的 **prompt 注入路线**(与 ccteam 红线相反)。

**vendor 自营已吞/将吞**(单 vendor 维度全线):Anthropic Managed Agents 2026-04 公测把 agent/environment/session/events 做成 REST($0.08/session-hour、vaults、memory_stores、self-hosted sandbox),Claude Code 铺满六 surface+托管 Routines+Agent View;Cursor Cloud Agents(run-based API、409 agent_busy、Automations cron+五种事件触发、self-hosted worker、Computer Use);Codex cloud(并行任务、@codex、Automations、订阅捆绑)。即:**spawn/托管/续命/事件流/计费/调度/多端入口在单 vendor 域内全是 vendor 自留地**;Terragon 与 Bloop(2026 年 2 月、4 月先后关停)已用尸体证伪「与 vendor 正面做纯云托管 / 免费驾驶舱」。留给第三方:跨 vendor 归一聚合、访问治理、自有 HITL/IM 面、prompt 资产治理、vendor 结构上不碰的「你自己机器上的多家混编」。

## 3. ccteam 的位置差异

**分工**:LAP 与 ccteam 共享赛道名词(session/key/budget/HITL)但执行域几乎不相交——LAP 站「vendor 托管 API 之上的 builder」:agent=DB 行、行为=system prompt 拼装、执行=一次性沙箱 run、状态=Postgres;ccteam 站「用户自己机器上的 vendor CLI 之上的终端壳」:执行=长命可恢复 session、行为=vendor 原生 `--agent` 自读(零注入)、状态=文件双 SoT、入口=用户既有 IM。竞争只在买家②的「控制中心预算」上发生;互补(LiteLLM/LAP 作 session 的 base_url 上游)零成本成立,但裁决已砍专属集成——base_url 只是 credential 普通字段。

**决断**:ccteam 的独特卖点=占住「**agent-CLI 层的 LiteLLM**」实证空位(模型层有对应物,agent-CLI 层没有)——把多家 vendor 的本机 CLI 会话收编进一个自托管、零入站、手机可驱动、扛重启、按 sid 寻址的常驻网关,在其上交付 vendor 永不做的**组织层**:principal(谁)×project(哪客户)×sid(哪会话)×vendor(谁家账)的围栏、记账、审批与回放。vendor 不做(中立聚合与其托管云、按席计费正面冲突——红队证伪潮后仅存的两类硬约束之一);LAP 做不了(无 PTY、恢复弱、注入路线,不服务订阅 CLI 形态);桥接器与桌面驾驶舱缺底座(无常驻、无 API、无 fleet)。一句话:不卖 agent 能力(vendor 的),不卖 builder(LAP 的),卖「多客户交付主机的信任与归因结构」。单点功能(IM、cron、cost 展示)全被证伪过,不再当卖点讲。

## 4. 需求清单

(26 个候选位收敛为 20 条,括号注明合并来源。)

### P0 — 入场券

**1. 零入站部署姿态+签名供应链**(合并两批同判 P0:零入站-纵深部署底座/姿态与签名更新)
做什么:web 默认 bind 127.0.0.1(`--expose` 显式升级)、`ccteam service install`(systemd+崩溃重拉)、`doctor --security` 审计(监听面/文件权限/token 强度)、install.sh 与自升级加 minisign 签名、SSH/Tailscale 隧道剧本。证据:买家②第一痛=安全运维;OpenClaw 事故链=同形态前车;LAP master key 缺省全放行=赛道普遍翻车。现状:TG long-poll 纯出站是结构优势,但 web 默认 `0.0.0.0:7331`、注释自认「reaches the LAN」(ccteam-web/src/lib.rs:59,86,已复核),install.sh 仅同渠道 sha256。差距:1-2 wave,零新架构。moat:「零入站默认+手机全功能遥控」只有 IM 出站网关为第一入口的架构能诚实承诺;单前端对手砍暴露面=砍功能。裁判:「出一次扫段事故=壳的全部信任卖点清零」「抄姿态=先变成 ccteam」。

**2. 同驾-回合仲裁**
做什么:网关 per-sid 回合锁——turn 飞行中另一前端/他人输入→排队/拒绝/显式 override,人话回执,锁落盘扛重启。证据:Cursor 把单飞行 run 做成 409 agent_busy=回合冲突是协议级问题;TG+web 双前端单人今天就能踩中,并发截断=「agent 行为诡异+白烧 token」。现状:gateway 零 busy/in-flight 处理;v0.8.10 file-backed stall SoT 原料已有。差距:S-M,集中 handle_text 单点。moat:仲裁只能住在全前端唯一汇合点;锁在网关不在通道,stream-json/tmux 共用。裁判:「moat-fit 满分……本批唯一可立即开工项」。

**3. 会话级 key 托管与计费身份绑定**
做什么:项目级 credential profile,spawn 按 sid 注入 vendor 原生 env(ANTHROPIC_API_KEY 等),操作者永不见明文;base_url/额外 env 作普通字段顺带。证据:key 托管是 LAP table-stakes 之一;OpenClaw infostealer 专偷配置文件 key;agency 多客户切号=日常摩擦。现状:零 key 管理(全仓 rg 0 命中),完全押宿主全局登录;注入管道现成(chat_spawn_env_owned,claude_tui.rs:307)。差距:约 1 wave。moat:chat→project→sid→credential 绑定链只存在于壳的 spawn 喉口;vendor 永不托管竞品的 key。裁判:「不做就不配自称该赛道的控制中心」——范围限定:P0 仅限「多客户 API-key 计费」子形态(②③主力订阅流量不过网关,订阅侧对应件=#7 per-sid 归因+#14 窗口视图,见 §7-3);订阅 OAuth per-session 复用为最薄子路径;诚实声明 same-uid 可读 env。

**4. 客户项目-软围栏**(硬依赖 #6 同挂多人触发线;P0=多人 onboarding 先决件,信号前不耗 wave)
做什么:principal→project slug allowlist 在 dispatch 单点强制,/cd、session 列表、turns 读取、web token 视图、cost 视图全过滤;诚实定位 best-effort 软边界(沿用 session-secret 红线表述)。证据:agency NDA 现实;LAP 把 per-user scoping 列为明示 non-goal;①级硬隔离(OpenHands VPC、Devin teamspace)价位与②错配——②价位段「够用围栏」无人供给。现状:任何过 ACL 的 sender 或持 web token 者可见可驾全部项目。差距:M;明确不做 SSO/RBAC/SIEM。moat:强制点=全前端共用的单 dispatch,一处生效全覆盖;vendor 看不见你的客户结构(利益冲突+信息位双重免疫)。裁判:「单人远程驾驶段位正被 vendor 吸收,幸存空间=多人×多客户组织层;围栏是放第二个 principal 进门的先决条件」。

**5. 项目即租户-OS 用户隔离**(P0 = 必须立项为下一主线,非一次落地)
做什么:每项目一个 Unix 用户,daemon 为唯一跨域特权代理;opt-in `project new --isolated`,per-uid CLAUDE_CONFIG_HOME(顺带 per-client 账单分离),SO_PEERCRED 把 session_* 调度门变硬。证据:88% 试点死于隔离/治理/合规(northflank);自家代码注释即需求书:「NO hard boundary…requires a per-agent OS user — v0.8.8-deferred」(mcp_session_tools.rs:46-51,已复核)。现状:单 uid 全信任,secret 只抬门槛。差距:大(大版本主线量级);opt-in 起步压首 wave 到中等。moat:「自托管+多 vendor+硬隔离」同时成立只有常驻 daemon 位置给得了;vendor 托管沙箱隔离的是他家云。裁判:「vendor 自营对单人便利是降维打击;在幸存空间里『客户 A 能 cat 客户 B 的 repo+凭证』对 agency 是 disqualifying」。

### P1 — 显著加深结构位置

**6. 多主体-身份网关(principal 一等原语)**(吸收 CUT「多操作员-范围与审批路由」唯一有效原子)
做什么:senderId→principal 映射、群聊绑默认 project、per-principal web token,入站 turn/出站 ledger/HITL 决议全盖 principal 戳。证据:买家② 2-5 人共用一台主机;landscape 空白①「Telegram/Lark 上与常驻 session 的持续双向 chat 无人占位」;Devin Teams $500/月;LAP「任何 key 与 master 等权」。现状:多前端已共用同一 handle_text+ledger;ACL 只是准入门,放行后人人等权;TurnRecord 无身份字段。差距:2-3 waves,全在 gateway/ledger 层,零红线冲突。moat:身份层只能长在全前端唯一汇合的 chokepoint;vendor 做多人=拉进它的 org 按席计费(商业模式硬约束);#4/#8/#12/#13 的公共复利基座。裁判:「本批唯一的结构位置乘法……启动前置=出现第一个真实 ≥2 人场景」;诚实记账:今天零多人付费信号,多人共用一份订阅可能踩 vendor ToS。

**7. 项目级成本归因账本+预算闸落 live dispatch**(合并 5 条同向裁决:项目级-成本归因与预算闸/项目预算-跨vendor计量/账单级-成本归因账本/项目预算闸-落到live-dispatch/项目级-预算硬墙——本清单最强收敛信号)
做什么:成本流水按 slug×sid×principal×vendor×model 归因,`GET /api/v1/cost?group_by=…`+CSV+IM `@ccteam cost <slug>`;budgets 加 per-project cap,dispatch/spawn 前置检查,触顶仅拒该项目新 turn(沿 auto-disable 例外语义,绝不 kill in-flight);spawn 可带 per-session max_usd 保险丝。证据:买家②三痛之 c 原文「成本按项目归因+预算上限」;LAP SpendLogs 留 team/org/agent 字段却无限额执行=最近竞品在强制上空白;CLAUDE.md 已承诺 budget auto-disable 但 live chat 路径未兑现=兑现自家空头支票,非新 feature。现状:原料齐(progress 带 slug+cost、turns 有 usage、Budgets schema 在 ccteam-cost),但 gateway.rs 零 budget 引用,enforce_budget 全在 daemon 不跑的 ccteam-flow(orchestrator.rs:1579,3155;纠正「项目预算-跨vendor计量」裁决误读——router.rs:303 的 budget_exceeded 是 @handle 转发跳数测试,与成本无关,live 闸门系首建非「机制已在」);TurnRecord 零 principal。差距:小-中,1-2 wave。moat:「project」是壳的概念域,vendor 只见自家 org/key;跨两家合并账+同一强制开关踩在利益冲突硬约束上;principal 维只有网关位知道。裁判:「投入产出比全场最高」;纪律:严守机械阈值闸,绝不向「降档省钱」smart-logic 生长;导出带估算口径标注。

**8. 归因-回放账本(per-project replay 导出)**
做什么:progress+turns+HITL 决议+cost 按时间轴合并为「谁让谁干了什么、谁批的、花了多少」可导出回放包(jsonl+md),REST+IM 一键出示客户;只归因出示,零智能分析。证据:按工时计费必伴对账纠纷(Devin ACU 先例);Anthropic 官方「SSE 无重放」、Cursor usage 无人类归因。现状:原料全在且 ccteam-owned;B1(chat_permission_resolved)sanctioned 未落地。差距:B1(S)+时间轴 assembler+导出(M);principal 维依赖 #6。moat:人×客户×session×vendor 归因只在网关汇合点存在;双 SoT 从恢复手段升级为可出示资产(invoice defense)。裁判:「精确避开上轮砍掉的『智能审计』」;honesty 修正:勿称 LAP 全内存态(runtime_events 有持久化),只押人与客户归因维度。

**9. 多机-舰队寻址(host 第三 facet)**
做什么:卫星机 `ccteam satellite` 出站 WS 连主 daemon(Cursor self-hosted worker 同款零入站语法),spawn 选 host,sid 全局唯一不变;分期:先 host registry+SSH spawn。证据:landscape 空白③「agent-CLI 层的 LiteLLM 缺位」;Cursor self-hosted worker(Brex/Notion)证明买家接受该部署语法;host=当下唯一诚实的隔离单元。现状:无(全仓零 multi-host 残留=干净起点);sid 不含 host=寻址天然位置透明。差距:大(独立 minor 主线量级,owner 拍板)。moat:壳的同构放大——护城河五要素逐项乘 N;vendor 的 self-hosted 必绑自家后端计费,不会出中立版。裁判:「moat-fit 全场最高……唯一不依赖被证伪论证模式的候选;不许整版本梭哈」。

**10. 沙箱-spawn-backend**
做什么:第三 spawn backend(`backend: sandbox`,bubblewrap/podman,项目目录 rw+vendor 二进制 ro),持久 sid/resume/CanonicalEvent 语义不变——产物是「可恢复的常驻沙箱 session」而非一次性 run。证据:ClawHub ~20% 恶意 skill 而 ccteam-hub 同形态分发第三方内容;tech-design 自认 skip-permissions 滥用仅靠软挡;E2B/LAP 沙箱 run-scoped、Sculptor 无常驻——「沙箱里的长命会话」市场缺位。现状:无沙箱,但 backend 枚举可扩展是既定架构(v0.8.11 计划中的双 adapter 方向)。差距:大(独立版本主线),Claude 单 vendor 起步,组合 vendor 原生原语而非自研隔离。moat:「persistent resumable sandboxed session」是独有交集,前提就是壳本体;与 #5 构成轻重两档隔离梯度。裁判:「五条里唯一前提就是壳本体的候选」。

**11. 守护进程-凭证代理**
做什么:daemon 成唯一长期凭证持有者:git/gh 走 git-credential helper 指向 mcp.sock 按 sid 发 per-repo token,vendor key 走官方 apiKeyHelper,bot token/web-token 从 agent 可读路径撤出;发放事件入 progress.jsonl。证据:今天任一 session 可 cat ~/.ccteam/imd/credentials.json、外传订阅 OAuth——偷 bot token=偷走整个 IM 控制面;OpenClaw infostealer 同形态实锤。现状:0600+masked 面只防网络/界面侧,不防同 uid 读文件。差距:中(1-2 waves 出第一档);完全闭环依赖 #5=隔离主线第一伴随 wave。moat:凭证按 sid 发放只有持久 sid 身份体系的常驻 daemon 能做;全走官方接口=零注入。裁判:「壳从消息路由器升级成凭证授权中枢」;先行版诚实标注 defense-in-depth。

**12. 跨人-会话交接(/handoff)**
做什么:`/handoff <sid> @同事`:driver 换人,进程不重启、context 不丢,新 driver 收定向通知+briefing 简报(hub skill 生成、合法 user turn=零注入),事件落 progress。证据:agency 跨时区轮班/夜间续跑;vendor 的「交接」全是同账号跨设备(Claude 移动→桌面、Happy/Omnara 中继),跨人=跨计费主体,vendor 伤按席收费不会做。现状:人人皆 driver 但无交接语义;B5 briefing skill 已 sanctioned 未落地(可独立先行)。差距:S-M;依赖 #6+#2。moat:sid 不绑 vendor 账号、寿命独立于人——「扛 daemon 重启」升级为「扛人下班」。裁判:「多人控制室的标志性动词」;范围修正:缺席范围收窄为「vendor-CLI 控制面赛道内」。

### P2 — 有价值、可后置(各配触发条件)

**13. 审批权-定向路由**:HITL 升级为 named gatekeeper+超时升级+principal 校验。证据:危险操作须老板签,LAP flat inbox 等权;现状:resolve 无 approver 概念;差距:M,依赖 #6;moat:批准门长在 ccteam 独占的 PermissionRequest→IM junction;裁判:「今天配对 chat 只有 owner 一人,问题不存在——身份网关落地且多人买家真实出现后的正确第二步」。
**14. 订阅额度-一等预算资源**:rate-window 建模为与 USD 并列的预算单位,per-sid 展示谁在烧窗口。证据:②③主力形态是订阅,真实稀缺是窗口;现状:cost 全按 API 价折算;差距:中;moat:订阅流量唯一观测点=本机 transcript=双 SoT 位置;裁判:「先 B2 零成本拿走前 60%,账本落地后以派生视图补全」(精度风险:窗口是账号级,ccteam 只见本机)。
**15. 不停机-自升级**:签名 self-update+升级编排+resume 自检+IM 播报。证据:cargo rebuild 断管 25 分钟一手事故;现状:底座(tmux 解耦/ledger/resume)已齐,无编排;差距:1-2 wave;moat:「升级穿越 session 无感」只有 tmux-decoupled+双 SoT 壳能承诺;裁判:「stream-json 重启丢 in-flight 未解决前承诺『无感升级』=承诺先于能力」——排 #1 之后同弧线。
**16. 状态可携-备份搬家**:project export/import 可校验 bundle+doctor --restore-check,诚实标注 vendor 侧 context 不可携。证据:Managed Agents「archive 永久只读」;现状:双 SoT 全文件态,tar 已兑现 80%;差距:1 wave;moat:LAP/vendor 要抄得重写状态模型;裁判:「不立产品叙事,作为『可恢复』红线从扛重启到扛换机的收尾件」。
**17. 同源-遥测面**:裁判已拆穿捆绑——rotation(turns/progress 零轮转=磁盘炸弹)即归 A 档卫生主线;全局合并 SSE(subscribe_events tee 已在)一日 polish 顺手;/metrics+「事实标准接口」叙事砍(Claude Code 已有 OTel 导出,pre-v1 零用户群钉不成标准)。
**18. hub 供应链-签名吊销**:index minisign 签名+revocations.json+安装回执。证据:ClawHub ~824 恶意 skill;现状:content sha256 已闭环,index 本身无签名;差距:1 wave;moat:签名+吊销把目录升级为信任机构;裁判:「条件触发型——截止线=与 v0.8.12 track-upstream 同 wave 或之前」。
**19. 范围化-访问令牌**:`token mint --project --scope ro|drive`。证据:给客户只读链接是 agency 日常,LAP gateway key 等权是反面;现状:web-token 单枚全权(token.rs);差距:1-2 waves;moat:scope 语法=壳的寻址体系(偏薄);裁判:「第一个 agency 真要时再做,后置零代价」。
**20. 模型档位-spawn 属性**:POST /sessions 与 /new 加 model 参数+role 默认档。证据:codex 已做 per-role 锁=需求确凿;现状:spawn 只收 role/vendor/permission_mode;差距:小;moat:零(透传),价值在补全 sid 计费形态;裁判:「挂在预算/账本链条后捎带做,绝不单独立项」。

## 5. 显式不做(全部 CUT)

- **订阅额度-准入调度**(限流感知队列/错峰):账户级限流上 vendor 结构占优(服务端看见全部 session,ccteam 只见自己 spawn 的子集,「唯一汇合点」地基裂),且信号形状检测=上轮砍掉的调度智能回潮。残值:限流后 deferred-retry 复用 pending_inject(max_defer_minutes 原语已在 core)的薄原子可留;vendor 限流信号成文档化 API 后可小原子重提。
- **多操作员-范围与审批路由**:与 #6/#13 同簇的不完整合并降级版,chat_id 当 actor 在群聊(最需要区分人处)恰好失效;唯一有效原子并入 #6。
- **交付-验收门**:代码交付物支配形态=PR,签收 chokepoint 在 repo host 而非网关,vendor 轨迹(Codex review、Factory review Droid)必吞;「待验收」状态机=被否的编排级批准变体;残值(人类 verdict)并入 B1 事件词汇。
- **litellm 直连-虚拟 key 绑定**:key 托管落地后只是 credential 两个普通字段;专属集成=替正做竞品控制中心的生态(LAP 即 LiteLLM 系)养习惯;「vendor 不路由竞品网关」被其官方 GATEWAY_MODEL_DISCOVERY 打脸。
- **成本出口-推回既有成本面**(OTLP/SpendLogs 导出):买家错位——个人/微 agency 没有既有 Grafana 成本面;导出是无回路的钉子,真有企业买家时数天可补。
- **会话级-出网策略账本**(egress proxy):vendor 已发货同款原语(claude sandbox allowedDomains+MITM proxy、codex landlock/seatbelt)=「vendor 不会做 X」证伪模式精确复现;env HTTPS_PROXY 是君子协定,挡不住其引用的 infostealer;残值后置进 #10 的 netns。
- **LAP table-stakes 余件处置(未立候选的三件,防论证悬空)**:**CRON** 不做——vendor Automations/Claude cron GA 已吞单 vendor 域+「daemon 不 tick」红线;等价物=系统 crontab+`POST /api/v1/sessions/{sid}/turn`(LAP 同款外部触发形态),一页 usage 剧本即关。**memory** 不做——「跨项目记忆走官方接口,只读不生成」红线直接关,知识层归 vendor 原生 CLAUDE.md/AGENTS.md。**MCP 透传/registry** 不做——`.mcp.json` 归项目自管,不建 registry。

## 6. 版本线衔接建议

- **v0.8.11(stream-json+壳加厚)→ 0.8.11.x**:#2 回合仲裁+#1 最小子集(bind 翻转+doctor --security)与「壳加厚」同弧线——仲裁锁在网关不在通道,正好骑 E3 故障×通道矩阵验证;落位 0.8.11.x 尾随小增量,不动已冻结 scope。#3 的 env 注入两通道共用,随 stream-json adapter 同步设计免返工;#17 的 rotation 即并入该弧线;B1/B2/B5 三个 sanctioned 零成本原子同窗顺手落(B5 可先行)。
- **v0.8.12(track-upstream 市场)**:#18 hub 签名吊销硬截止线=与 v0.8.12 同 wave 或之前;市场放大的第三方内容风险同时是 #10 需求来源,但沙箱不绑 0.8.12 排期。
- **v0.9 主线 =「多客户信任底座」,两段落地**:前段 **#3 key 托管 → #7 账本+预算闸**(slug×sid×vendor 维,均无 #6 依赖,单人买家即刻受益);#8 紧随 #7 同弧线(共享 assembler,B1 为其 S 级前置,#20 捎带)。后段 **#6 身份网关 → #4 软围栏 → #7/#8 的 principal 维 → #12**,整段挂「第一个真实 ≥2 人/agency 信号」触发,信号未现不消耗 wave。#11 先行档(per-sid git credential+发放审计)给独立小槽,硬保证档绑 #5 首伴随 wave。多机(#9)在 v0.9 内只做 host registry+SSH spawn 第一步+sid/host 语义冻结——放大一台无围栏、无 key 隔离、不按客户记账的机器=放大信任缺口,故信任底座先行、不整版本梭哈。
- **对裁决的显式修正(#5)**:原判「必须立项为下一主线」;处置=「v0.9 并行立项+设计冻结(#4 软围栏即语义前奏:principal→slug 逻辑边界先行),物理 uid 兑现为 v0.10 主线」——字面成立,不与信任底座抢 wave;#10 排期归 owner(§7-1)。

## 7. 开放问题(owner 决策,≤5)

1. **mainline 槽位次序**:v0.9 信任底座之后,四个主线量级竞争者——ccteam-flow / 多机 #9 / uid 隔离 #5(默认 v0.10)/ 沙箱 #10——先后需正式裁决,含红队遗留的「flow 要么上主线要么停止用它背书」。
2. **多人触发线确认**:确认 §6 后段触发线(#6→#4→principal 维→#12,连带 #13:第一个真实 ≥2 人/agency 信号启动,信号前不为②预付 wave)——「#4 标 P0×零多人信号」矛盾的收敛解。
3. **订阅 ToS 边界**:多客户/多人共用一份 Max 订阅的产品口径:#3 已限 API-key 主线规避大头,「复用既有订阅」卖点的诚实范围仍需一句官方表述(与 #3 范围限定/#14 表里)。
4. **商业形态**:OSS+managed hosting(RunMyClaw 先例)还是 OSS+低价订阅?决定 #1 做到「自托管文档级」还是「一键托管镜像级」,也决定③漏斗→②的转化路径。
5. **#5 兑现节奏确认**:确认 §6 两段式处置(v0.9 立项+设计冻结、v0.10 物理 uid 主线);若 v0.10 排不下,是否接受软围栏长期作唯一边界及其文档诚实成本。

## 8. Owner 裁定(2026-06-10,TG 1106)—— 拓扑纠正与重排

> 本节为 owner 对 §1–§7 的事后裁定,优先级高于上文;上文保留原样作论证存档。

**拓扑纠正**:ccteam 跑在**单租户独享的 agent sandbox** 里 —— 一个客户/一个交付 = 一个独享 sandbox,**不在单 daemon 内按 project 做多租户**。客户隔离由 sandbox 边界(infra 层)白送,不靠 daemon 内围栏。

**失前提即砍/降**:
- **#4 客户项目-软围栏、#5 项目即租户-OS 隔离:P0 → CUT**(作为客户互防的前提消失;#5 残值 = sandbox 内部纵深防御,可后置不立项)。
- **#6 身份网关、#13 审批路由:降档收窄** —— 不再是"同机多客户互窥防线",收敛为集群入口的「谁能驱动哪个 sandbox + 操作署名」。
- §7-2(多人触发线)、§7-5(#5 节奏)随之失效大半;§7-1 主线竞争格局改写(见下)。

**升格**:**#9 多机-舰队寻址 从 P1 升为候选主线** = 「**sandbox 集群化 agent 编排**」:一个控制面 / 一个 IM 入口,对 N 个单租户 sandbox 做 spawn / 寻址(sid 全局)/ 恢复 / 记账;#10 沙箱-spawn-backend 与之同向(sandbox 供给侧)。归因模型反而更干净:**一个 sandbox = 一个客户 = 一条账**,per-sandbox 成本/回放天然按客户分。

**不受影响、继续有效**:#1 零入站姿态+签名(每个 sandbox 都要)、#2 同驾回合仲裁(per-sid 语义不变)、#3 凭证注入(per-sandbox 反而简化)、#7 账本+预算闸(归因单位从 project-inside-host 改为 sandbox,简化)、#8 回放账本、#12 /handoff(集群层)、#15/#16/#18。

**具名基底**(owner 同步给出,TG 1107):[kubernetes-sigs/agent-sandbox](https://github.com/kubernetes-sigs/agent-sandbox)(v0.4.6,SIG Apps)—— `Sandbox` CRD:单例有状态工作负载、稳定网络身份、扛重启持久存储、pause/resume 生命周期、gVisor/Kata 隔离;扩展 CRD `SandboxTemplate`/`SandboxClaim`/`SandboxWarmPool`;Python client。**与 ccteam 红线的结构同构**:Sandbox 的 pause/resume + 稳定身份 + 持久存储 ≅ ccteam 的 resume-by-sid + 空闲释放 + 双 SoT —— k8s 把「可恢复的常驻会话」做成了集群原语,ccteam 把它做成了单机原语;集群编排方向 = 两者对接(sid ↔ Sandbox CR),#9+#10 合并为同一条主线的两面(寻址面 + 供给面)。

**待下轮 PRD 决断的关键分叉**:① ccteam-per-sandbox(每租户一个完整 ccteam,上面另起薄控制面)vs ② ccteam-as-control-plane(网关居集群入口,session spawn 进 k8s Sandbox,`backend: k8s-agent-sandbox` 作第三 spawn backend)。②保留壳的全部结构位置(网关唯一汇合点 / sid 寻址 / IM 单入口),①把控制面问题推给未知新层 —— 倾向②,owner 拍板。**§6 版本线待下轮 PRD 按本裁定重排**(信任底座的"多客户在一台机"段落让位于集群编排方向)。
