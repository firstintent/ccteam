# v0.8.18 PRD — Loop 地基(driving cockpit + identity)

> 状态:**讨论稿(doc-first,代码未动)**。owner 定调(2026-06-21):**loop 移到下一版本;本版只做 loop 的地基**,且每块都自己就有用。
> 原型(**基于真实 `ChatConsole` 壳**:sidebar + topbar + main,照搬 `web/src/index.css` 的 @theme 配色/Geist 字体/status 状态色 —— 增量不重画):**[`v0818-real-shell.html`](prototype/v0818-real-shell.html)** = 柱1(环境 + 舰队)+ 柱2(身份 scope)一屏可点,**视觉以此为准**。早期 phone-shell 草图(`environment-cockpit` / `multi-user-soft-partition` / `loop-ops-console`)仅留作内容参考,配色/布局已被上面这版取代。
> loop 本身的设计见 [`../../research/loop-engineering-ccteam.md`](../../research/loop-engineering-ccteam.md)(**下一版**)。

---

## 0. 一句话

loop 是大赌注,单独一版做;但它的底座该先在。**本版 = loop 的「看 + 管 + 谁的」底座** —— 两根柱子,loop 本身不在内,但每根现在就有用:

- **柱 1 · 驾驶舱(看 + 管)** = 环境体检 + 舰队/成本视图 → 就是 loop 运维台的骨架,本版先显示 **session**,下一版 loop 直接 slot 进来。
- **柱 2 · 身份(多用户档 0)** = ACL own-only + 个人 scope → 既治当前「会话串」,又是无人值守多用户 loop 的硬前置。

> 为什么是地基:loop 给 ccteam 的价值 = **看 + 管 + 卡门**。这两根柱子正是「看/管/身份」的底座。它们 ship 了,下一版 loop = 往现成驾驶舱里塞 loop + 加 on-ramp + oracle-diff 门,风险大降。

---

## 柱 1 · 驾驶舱(看 + 管)

### 1A. 环境体检 —— `GET /api/v1/environment`

把现在只探 `--version` 二态、写死 claude/codex 的 `capabilities.rs` 升级成真正的环境报告:每 vendor 一卡(装了吗 path+**version** / 登录了吗 / **ccteam MCP 注册了吗** / hook+settings / daemon home-drift)+ 红黄绿 + 缺啥给**可复制命令**。

- **唯一可从 web 写的** = ccteam 自己的足迹(一键注册 MCP,重跑 `ccteam config` 那段,幂等);**绝不**从 web 写 vendor 登录/key、**绝不**从 web 装 CLI(执行面红线)。
- vendor-可扩展(`AgentVendor` + 每 vendor `ProbeSpec` 数据)+ 手动 re-probe(破 daemon-终身 cache)。
- **为 loop 准备**:下一版「云端起跑一个 loop on vendor X」前,得先知道 X 装好/登录/可用。

### 1B. 舰队 + 成本视图

web 里一个真正的 fleet 视图:列**所有 session** 的状态(live/idle/活动)+ per-session/项目 **成本** + 今日 spend/budget。后端 = 扩 `GET /api/v1/status`(已有 sessions live/idle + 今日 cost/budget);前端 = 一个 fleet 卡片视图。

- **为 loop 准备**:这就是 `loop-ops-console.html` 的骨架 —— 本版卡片显示 **session**(预言机/门那几栏先空着或 N/A),下一版 loop 把「预言机 🟢🔴⏸ + 等哪道门」填进同一批卡片。**先建壳,后填 loop。**
- 现在就有用:你终于能一眼看全 N 个 session + 卡预算(loop 来之前就值)。

---

## 柱 2 · 身份(多用户软分区 · 档 0)

详 [`../../research/multi-user-soft-partition.md`](../../research/multi-user-soft-partition.md)。共用一个 daemon、同一个 OS 账号下,**最小**去掉「会话串」:

- **档 0(本版必需,几行)**:ACL 收 **own-only** —— 删 `chat_can_access`(`gateway.rs:1219`)的「同 project 互看」+「web-operator 通看」两条漏。IM 本就 per-chat(Telegram `chat_id` = 免费身份),立刻互不可见。**零新字段/token,不碰 `ccteam config`**(bot 是 daemon 级,一个 bot 多 chat)。
- **档 1(本版选配,web)**:web 单 token = 单操作员通看。复用 web 已有「一次绑一个 chat」(`state.rs:96`)→ 每人预绑自己 chat 的个人链接 + own-only scope。或 v0:web 当 admin 面,其他人用 IM。
- **诚实红线**:同 uid = **软隔离非安全**(同机互读),UI 横幅标注;不拆进程/不开沙箱。
- **为 loop 准备**:无人值守的多用户 loop 跑起来前,必须先有身份分区,否则 A 的 loop 被 B 看见/操作。`chat_id` 这根身份键也是将来「真隔离 = 路由到自己沙箱」的同一根。

---

## 范围切口 / 不做

| 做(本版) | 不做(留给 loop 版 / 不碰) |
|---|---|
| 环境体检(只读 + 仅写 ccteam 自身足迹) | ❌ 从 web 写 vendor 登录/装 CLI |
| 舰队/成本视图(显示 session) | ❌ loop 运维台的 loop 专属栏(预言机/门)—— 下一版填 |
| 多用户档 0(ACL own-only)+ 档 1 选配 | ❌ on-ramp(loop-skill 库 + 云端起跑)· oracle-diff 门 · loop 版本管理 —— 得有 loop 才有意义 |
| — | ❌ 跨 vendor 路由 · 拆进程/沙箱 · 改 session 存储 |

**红线**(同 CLAUDE.md §三):ccteam executes nothing(除自身 MCP 注册);绝不碰 `settings.json`;No-prompt-injection(本版不碰 spawn);软隔离诚实标注非安全。

---

## 验收

1. `GET /api/v1/environment`:claude 装好/codex 没装 → `claude=ready`+`codex=not_installed` 带 version;`POST …/register-mcp` 幂等。
2. 舰队视图:列全部 session 的 live/idle + 成本;卡片结构预留 loop 专属栏(下一版填)不报错。
3. 多用户档 0:A、B 各 IM pair → 各自 `/use`/`/cd` 只能碰自己的 session;旧「同 project 互看」漏堵上。
4. 探测/分区用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)+ 假 chat_id,不依赖真 binary。
5. baseline 不退:`cargo test --workspace --exclude ccteam-web` ≥ 现状;clippy 0 warning;`cargo fmt --all` clean;vitest/Playwright 不退。

---

## 落地姿势

doc-first(本文)→ owner review 范围 → 一个 minor(v0.8.18):① 后端 `environment` 路由 + register-mcp + `status` 扩 fleet/cost;② SPA 环境面板 + 舰队视图;③ gateway ACL own-only(+ 选配 web 个人 token)。直接在 dev 落、按 [[ship-flow]] 不发 tag。**下一版**接 loop(运维台填 loop 栏 + on-ramp + oracle-diff 门)。

> 本轮只讨论 + reframe PRD,代码不碰。
