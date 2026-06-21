# v0.8.18 PRD — Loop 地基(driving cockpit + identity)

> 状态:**已落地 dev(v0.8.18,无 tag,等 owner review)** —— 实现总结 + 验收结果见 [`handoff.md`](handoff.md)。**档1(per-user web token)按「选配」延后到后续 patch。** owner 定调(2026-06-21):**loop 移到下一版本;本版只做 loop 的地基**,且每块都自己就有用。
> 原型(**基于真实 `ChatConsole` 壳**:sidebar + topbar + main,照搬 `web/src/index.css` 的 @theme 配色/Geist 字体/status 状态色 —— 增量不重画):**[`v0818-real-shell.html`](prototype/v0818-real-shell.html)** = 柱1(控制台 + 主机)+ 柱2(身份 scope)一屏可点,**视觉以此为准**。早期 phone-shell 草图(`environment-cockpit` / `multi-user-soft-partition` / `loop-ops-console`)仅留作内容参考,配色/布局已被上面这版取代。
> loop 本身的设计见 [`../../research/loop-engineering-ccteam.md`](../../research/loop-engineering-ccteam.md)(**下一版**)。
> **🚀 开发提示词**:[`dev-goal-prompt.md`](dev-goal-prompt.md) —— 新会话 `/goal` 整版实现(owner review 原型后启动)。

---

## 0. 一句话

loop 是大赌注,单独一版做;但它的底座该先在。**本版 = loop 的「看 + 管 + 谁的」底座** —— 两根柱子,loop 本身不在内,但每根现在就有用:

- **柱 1 · 控制台 + 主机(看 + 管)** = ① **Status 长成控制台**(每条 session 状态+成本;**不是新页,是旧 Status 长大**,「舰队」并入它)→ loop 运维台的骨架,下一版同一批行加「预言机/门」两列 ② **主机页(Hosts)**(`/api/v1/hosts`;每台机器的 hostname/规格 + 上面接进来的 agent/MCP)。
- **柱 2 · 身份(多用户档 0)** = ACL own-only + 个人 scope → 既治当前「会话串」,又是无人值守多用户 loop 的硬前置。

> **去重(owner 反馈 1222)**:原稿把一个东西切成「环境 / 舰队 / Status」三页且重复。收敛成 **一个会长大的控制台(Status→fleet→loop 运维台,同一面)+ 一个主机页(Hosts,setup,与 runtime 区分)**。原型 `v0818-real-shell.html` 的「预览 loop 版」按钮演示同一批行就地长出 loop 列。

> 为什么是地基:loop 给 ccteam 的价值 = **看 + 管 + 卡门**。这两根柱子正是「看/管/身份」的底座。它们 ship 了,下一版 loop = 往现成驾驶舱里塞 loop + 加 on-ramp + oracle-diff 门,风险大降。

---

## 柱 1 · 控制台 + 主机(看 + 管)

### 1A. Status 长成控制台(fleet)—— 不是新页,是旧 Status 长大

今天 `StatusView` 给的是一眼概览(daemon 健康 + 会话数 + **今日总成本**),看不到单条。本版把它**就地长成 fleet**:`会话` 卡列出每条 session 的状态 + **per-session 成本**(原稿叫「舰队」的内容,**并入 Status,不另开页** —— owner 1222 指出二者重复)。后端 = 扩 `GET /api/v1/status`(已有 sessions live/idle + 今日 cost/budget,补 per-session cost);前端 = 在现有 `StatusView` 的会话卡上加成本列。

- **为 loop 准备(关键)**:这就是 loop 运维台的骨架。本版每行是 **session**;下一版 loop 在**同一批行**就地加两列「预言机 🟢🔴⏸ + 等哪道门」→ session 行变 loop 行。**同一个面,先建壳后填 loop**(原型「预览 loop 版」按钮演示)。
- 现在就有用:一处看全 N 条 session 在跑啥 + 各花多少 + 卡预算(loop 来之前就值)。

### 1B. 主机页(Hosts)—— `GET /api/v1/hosts`(原 `environment` 改名)

**机器是主轴**(owner 1236:`environment` 框窄了,它说的就是「这台机器」,而将来分布式多台)。host-first + 与 Status 的 runtime 面区分(主机=装没装/连没连的 setup,Status=在跑啥/花多少的 runtime;前者很少变,后者一直变)。把现在只探 `--version` 二态、写死 claude/codex 的 `capabilities.rs` 升级成 host-keyed 报告:

- **资源**:`GET /api/v1/hosts`(列所有机器)+ `GET /api/v1/hosts/{host}`(某台详情)。**host id** = `local`(this machine)/ hostname;与已有 host 轴一致(session 预留 `host` 字段,默认 `local`)。**今天一台**(this machine);**将来分布式** → 列所有 satellite,每台一条,session 也标在哪台 host 跑。
- **每台机器**:hostname + 规格 + ccteam 版本/端口;其上每 agent 一卡(装了吗 path+**version** / 登录了吗 / **ccteam MCP 注册了吗** / hook+settings)+ 就绪/需配置/未安装 + 缺啥给**可复制命令**(这就是原「接入」的内容,降成每台 host 的一段)。
- **唯一可从 web 写的** = ccteam 自己的足迹(一键注册 MCP,重跑 `ccteam config`,幂等);**绝不**从 web 写 vendor 登录/key、**绝不**从 web 装 CLI(执行面红线)。
- vendor-可扩展(`AgentVendor` + 每 vendor `ProbeSpec`)+ 手动 re-probe。
- **为 loop 准备**:下一版「云端起跑一个 loop on vendor X @ host Y」前,得先知道 X 在 Y 上装好/登录/可用。

---

## 柱 2 · 身份(多用户软分区 · 档 0)

> **新用户操作步骤 + 可点走查**:[`multi-user-onboarding.md`](multi-user-onboarding.md) + 原型 [`prototype/multi-user-walkthrough.html`](prototype/multi-user-walkthrough.html)(切「新用户首次 / @bob / @alice」体验各看各的私有世界)。**owner 这关先过这俩**。设计详 [`../../research/multi-user-soft-partition.md`](../../research/multi-user-soft-partition.md)。

共用一个 daemon、同一个 OS 账号下,**最小**去掉「会话串」:

- **档 0(本版必需,几行)**:ACL 收 **own-only** —— 删 `chat_can_access`(`gateway.rs:1219`)的「同 project 互看」+「web-operator 通看」两条漏。IM 本就 per-chat(Telegram `chat_id` = 免费身份),立刻互不可见。**零新字段/token,不碰 `ccteam config`**(bot 是 daemon 级,一个 bot 多 chat)。
- **档 1(本版选配,web)**:web 单 token = 单操作员通看。复用 web 已有「一次绑一个 chat」(`state.rs:96`)→ 每人预绑自己 chat 的个人链接 + own-only scope。或 v0:web 当 admin 面,其他人用 IM。
- **诚实红线**:同 uid = **软隔离非安全**(同机互读),UI 横幅标注;不拆进程/不开沙箱。
- **为 loop 准备**:无人值守的多用户 loop 跑起来前,必须先有身份分区,否则 A 的 loop 被 B 看见/操作。`chat_id` 这根身份键也是将来「真隔离 = 路由到自己沙箱」的同一根。

---

## 界面一致性(owner 1240/1241)

多用户 + 多机 落地后,UI 要整体跟上:

- **菜单语言**:导航标签随界面语言渲染,**默认中文**(「主机」),可切英文(「Hosts」)。
- **个人设置 vs 全局设置(两个面分清)**:
  - **点头像 → 个人设置(per-user,存身份名下)**:显示名 · 头像 · **界面语言(中文 / English,默认中文)** · 登出。语言归个人,从头像进。
  - **底栏「设置 Settings」(global/admin)**:IM token(telegram/lark)· 预算 · **用户管理**(列租户 + `ccteam user add` 铸 web 链接)。主机/Hosts 是独立页,不进 Settings。
- **i18n 范围(诚实)**:双语导航 + 头像里的语言开关(UI 件)**便宜,本版做**;但「选 English → 整个 UI 全英文」= 真 i18n(上框架 + 抽所有字符串翻译 + 维护两份文案),**分阶段**:本版先骨架(导航 + Settings + 关键页可切),全量 i18n 当独立小版本推。

> 原型 `v0818-real-shell.html` 已含:点头像弹个人设置(切语言 中文/English,导航跟着切,默认中文)+ 全局 Settings 页(IM/预算/用户管理)。

---

## 范围切口 / 不做

| 做(本版) | 不做(留给 loop 版 / 不碰) |
|---|---|
| Status 长成控制台(per-session 成本,显示 session) | ❌ loop 运维台的 loop 专属栏(预言机/门)—— 下一版同行加 |
| 主机页 Hosts(`/api/v1/hosts`,per-host + hostname;只读 + 仅写 ccteam 自身足迹) | ❌ 从 web 写 vendor 登录/装 CLI |
| — | ❌ 另开「舰队」页(并入 Status)· `environment` 命名(改 `hosts`,host-first) |
| — | ❌ 真·分布式多 host 调度(host 轴本版只「列」,起跑/路由留后)|
| 双语导航 + 头像个人设置(语言/显示名/头像)+ 全局 Settings(IM/预算/用户管理) | ❌ 全量 UI i18n(每页每条提示翻译)—— 独立小版本 |
| 多用户档 0(ACL own-only)+ 档 1 选配 | ❌ on-ramp(loop-skill 库 + 云端起跑)· oracle-diff 门 · loop 版本管理 —— 得有 loop 才有意义 |
| — | ❌ 跨 vendor 路由 · 拆进程/沙箱 · 改 session 存储 |

**红线**(同 CLAUDE.md §三):ccteam executes nothing(除自身 MCP 注册);绝不碰 `settings.json`;No-prompt-injection(本版不碰 spawn);软隔离诚实标注非安全。

---

## 验收

1. `GET /api/v1/hosts`:列出 this machine(host=local)+ 其 agents;claude 装好/codex 没装 → `claude=ready`+`codex=not_installed` 带 version;`POST …/register-mcp` 幂等。
2. 舰队视图:列全部 session 的 live/idle + 成本;卡片结构预留 loop 专属栏(下一版填)不报错。
3. 多用户档 0:A、B 各 IM pair → 各自 `/use`/`/cd` 只能碰自己的 session;旧「同 project 互看」漏堵上。
4. 探测/分区用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)+ 假 chat_id,不依赖真 binary。
5. baseline 不退:`cargo test --workspace --exclude ccteam-web` ≥ 现状;clippy 0 warning;`cargo fmt --all` clean;vitest/Playwright 不退。

---

## 落地姿势

doc-first(本文)→ owner review 范围 → 一个 minor(v0.8.18):① 后端 `hosts` 路由(`/api/v1/hosts`,host-keyed)+ register-mcp + `status` 扩 fleet/cost;② SPA 主机页 + 控制台 fleet(Status 长大);③ gateway ACL own-only(+ 选配 web 个人 token)。直接在 dev 落、按 [[ship-flow]] 不发 tag。**下一版**接 loop(控制台填 loop 栏 + on-ramp + oracle-diff 门)。

> 本轮只讨论 + reframe PRD,代码不碰。
