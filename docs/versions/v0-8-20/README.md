# v0.8.20 — 多租户深化:CLI 项目归属 + per-tenant IM bot + 登录链接可见

> **Status: LANDED**(dev,doc-first → 4 wave 实现,owner 2026-06-25)。**tag HELD**;daemon 需**重部署** + SPA 需**重 build** + 用户 `/mcp` 重连才上线。
> 建立在**档1**(per-user web,v0.8.18)之上:`~/.ccteam/tenants.json` 租户注册表 + 单 token→`Identity{admin|tenant}` + 项目归属 ACL(`ProjectState.owner`,web 建项目盖 `web:<tenant>`,session 继承 project 归属)。

## 落地概览(4 wave,全绿)

| Wave | 内容 | 关键文件 |
|---|---|---|
| **W1** | `ccteam init --owner`(裸值→`web:`、`:` 原样、re-init 覆盖**无需 `--force`**、轻校验 WARN) | `ccteam-cli/src/{main,commands}.rs` |
| **W2** | 归属 bug 修(auth `URL→cookie→Bearer`,cookie 优先 Bearer 回退)+ **F3** 登录链接(`GET /users/{id}/link` + `ccteam status` + Settings 复制)+ **F4** 建-session 默认无角色 + 按身份分级运行时/角色 + **beta-gating 规范**(→ CLAUDE.md §五.8) | `ccteam-web/src/{auth,routes/users,routes/openapi}.rs` · SPA `ChatConsole/SettingsPage/usersApi` |
| **W3** | Tenant IM 凭据模型(`TenantTelegram`/`TenantLark`,0600)+ 自助 REST `PUT /me/im`(getMe 校验,replace 语义)+ admin `PUT /users/{id}/im` + Settings「我的 IM bot」 | `ccteam-core/src/tenants.rs` · `ccteam-web/src/routes/users.rs` · SPA `SettingsPage` |
| **W4** | **per-tenant IM bot 监听/路由/热重载**(核心):channel `"<platform>@<tenant>"`、`build_tenant_channels` fan-out、`chat_can_access` 租户隔离、`platform_of` ACL、changed-scope 热重载 + web PUT 触发 | `ccteam-im/src/{daemon,gateway,transport/*}.rs` |

> **DEFERRED(owner review):** ① web↔IM **收敛**(租户 bot 看自己的 web session)—— gateway 把 web session 统一记 `web:web-api`(非 per-tenant),收敛需更大改,本版做**隔离**(租户 bot 只见自己 IM session)。② **全局 bot「废迁」是 reframe 非 literal**:不写迁移代码(pre-v1.0 规则),全局 `credentials.json` bot **不再共享**(租户各自有 bot),它只服务 admin。详见末尾 Handoff。

---

## 背景 / 动机

档1 落地了 per-user **WEB**,但还有缺口:① CLI `ccteam init` 建的项目 `owner=None`,没法指给租户;② IM 仍是单个**全局 bot**,所有人共用,web↔IM 复联 deferred;③ admin 看不到各租户的登录链接(创建时只回一次);④ **bug**:web 新租户建项目误归到 admin。本版把多租户从 web 推到 CLI + IM,并修这个归属 bug。

**诚实范围(沿用档0/档1):** 单 OS-uid 下是**软隔离(UX/路由),非安全边界**(同 uid 仍可读他人文件/`/proc/<pid>/environ`)。per-tenant bot 隔离的是 IM **路由 + 可见性**;真隔离 = per-user OS user / sandbox(仍 deferred)。

## 决策(owner 已定 2026-06-25,msg 1438)

1. **全局 bot:废 + 迁** —— 彻底废掉全局 bot,所有 IM 都 per-tenant;owner 现有全局 bot **迁移**成 owner 这个租户的 bot(同 token 不能两处用,是迁不是并存)。admin 也是一个有自己 bot 的租户。
2. **bot token 存储:明文** `tenants.json`(0600 权限,不加密)。
3. **注册:自助** —— 租户在 web 自己设自己的 bot(admin 也可代设)。
4. **IM 范围:所有 IM 并列** —— telegram + lark(+ 其余 channel)平等支持,不是 telegram 优先。
5. 资源(N 个监听):小 N 无压力,实现时按需懒启动,不设特殊上限。

---

## Feature 1 — `ccteam init --owner <value>`(支持覆盖)

CLI 起手把项目指给某租户。
- `ccteam init --owner <web:<tenant> | channel:<chat_id>>` → 写 `ProjectState.owner`。
- **归一化**:值含 `:` 原样;裸值(租户 id)前缀补 `web:`。空 → 未指定。
- **覆盖语义**:带 `--owner` → 覆盖已有 owner(re-init 也覆盖,**不需 `--force`**);不带 → 保留现有(新建=None,re-init=原值不清零)。
- 轻校验(不阻断):`web:<tenant>` 非已知租户 → stderr WARN,仍写。
- 回执显示 `owner: <value>`。
- 落点:`main.rs` `Init` 变体 + `commands.rs` `InitOptions` → 透传 `owner: Option<String>` 进 `bootstrap_project_at_dir`/`bootstrap_project`(projects.rs;grep 全 caller 传 `None`)→ set/override。规模小。

## Feature 2 — per-tenant IM bot(每用户自己的 IM bot)

每租户跑自己的 IM bot,不再共用全局;一个租户的 bot 只见/驱动它自己的会话。
1. **租户 bot 凭据** —— 扩 `Tenant`(tenants.json)加 `telegram_bot_token` / `lark_app{app_id,app_secret}`(明文,0600)。**所有 IM 并列**(telegram + lark 同等)。
2. **多监听** —— daemon 为每个租户 bot 起一个监听(N 个 `getUpdates` / lark WS),复用现有 provider 按租户 token 参数化;transport 从「单 bot」改「按租户多 bot 实例」。懒启动。
3. **inbound 路由 + 身份** —— 消息落租户 X 的 bot → `Identity{tenant:X}`(复用档1)→ 路由 X 的 current session、ACL 按 X。
4. **注册(自助)** —— 租户登录 web 在 Settings → 我的 IM bot 填 token → 新 REST(如 `PUT /api/v1/me/im`),`getMe` 验证再存 + 起监听;admin 也可代设(`/api/v1/users/{id}`)。
5. **热重载** —— 增改租户 bot → daemon 起/重启那个租户的监听(扩 `ccteam/reload`/`request_im_reload` 从单全局到 per-tenant 增删),不重启 daemon。
6. **配对/allowlist** —— 每租户 bot 自己的 allowlist(租户 chat_id);`/pair` 按 bot。
7. **全局 bot 退役 + 迁移** —— 废全局;owner 现有全局 bot 迁成 owner 租户的 bot(`ccteam config` 的全局 IM token 这条退役或转成「设 admin 租户的 bot」)。
8. **web↔IM 复联** —— per-tenant bot 天然解决:租户的 bot=其身份(tenant id),web/IM 同一 `Identity{tenant}`,落地档1 留的 `linked_chat`。

## Feature 3 — 多用户登录链接可见(`ccteam status` + admin 后台)

admin 随时看**所有租户的登录链接**(`?token=ccteam:<hex>`),两处:
- **`ccteam status`(CLI)**:本机 admin 跑 → 列每个租户 + 登录链接(读 `tenants.json`)。
- **admin web 后台**:用户管理 UI 每行展示登录链接 + 复制(不只创建时回一次)。

**改了档1 纪律(对 admin 放宽):** 档1「列表永不回 token」→ 本版**对 admin** 可回(admin 受信)。`GET /api/v1/users` 对 **admin** 序列化带 link;`ccteam status` 本机=admin。**租户仍永看不到别人的 token**(自助接口只回自己的)。

## Feature 4 — web 建 session:默认无角色 + 按角色分级展示厂商/协议

**目标(owner msg 1440):** web 建 session 表单**默认无角色**(roleless);厂商/协议选项**按用户角色分级**:
- **普通用户(tenant)**:只展示 **claude / codex** 两个厂商;协议固定 **stream-json**(不露 terminal/rmux);默认无角色。简洁、只给生产稳定路径。
- **admin**:展示**全部** —— 所有厂商 + **协议轴(stream-json | terminal/rmux)** + 角色选择。

落点:SPA 建-session 面按 `GET /api/v1/me` 的身份(admin|tenant)**条件渲染**;**纯 UI 门**(后端创建路由不变,仍按现有鉴权 —— 不是安全边界,见下「开发规范」)。这是下面规范的一个实例(rmux/terminal = 高级/beta → admin-only)。

## 开发规范(新,本版确立 → 落 `CLAUDE.md`)

**owner msg 1440:** **beta / 新 / 不稳定功能只对 admin 账号展示;普通用户只展示生产稳定功能。仅在 UI 层限制**(后端照常服务,**不是**安全/权限边界 —— 真正的权限门仍走 `deny_non_admin`/`can_see_project` 等既有 ACL)。
- 机制:SPA 按 `GET /api/v1/me` 的角色(admin|tenant)show/hide;每个「beta」功能挂一个标记(如一个 `beta: true` 列表 / 一个 `useMe().isAdmin` 守卫)。
- 意义:新功能先只给 admin(owner)灰度自测,普通用户始终只见生产稳定面;降低新功能对普通用户的暴露面。
- 例:rmux/terminal 协议、其余实验特性 = admin-only;claude/codex stream-json = 全员。
- **dev session 落地时把这条写进 `CLAUDE.md`**(项目规范段),后续新功能默认遵守;并约定:beta→stable 毕业时移除该 UI 门。

## Bug 修复 — web 新租户建项目误归 admin(deployed,本版修)

**症状(owner msg 1438):** 新用户 web 建项目后归到 admin(应 `web:<tenant>`)。

**根因分析(本 session 排查,deployed `5314c36`):**
- 建项目路由 `routes/projects.rs:138` `owner = identity.web_owner()` —— **正确**(`web_owner()`:admin→`web:web-api`、tenant→`web:<id>`,`auth.rs:155`)。
- `auth_layer`(`auth.rs:334`)身份优先级:**① Authorization header (Bearer) > ② URL-shim `?token` cookie > ③ cookie**(auth 已启用;loopback 短路仅 `!auth.enabled` 时,本机 auth 开着不触发)。
- SPA **同时**带 same-origin cookie **和** `Authorization: Bearer <token>`(fetchInterceptor monkey-patch,`configApi.ts:14`)→ **header 赢**。
- ⇒ **最可能根因**:SPA 的 Bearer 没随当前登录(租户)重取,带的是**缓存/残留的 admin token**(上次 admin 登录),header 盖过新设的租户 cookie → 建项目 identity 解析成 **admin**。

**下个 session 要做:** ① 服务端建项目处 **log 解析出的 Identity** 实证;② 修 SPA token:Bearer 跟当前 cookie 同源(每次登录重取 `/api/v1/auth/token`、别跨登录缓存),或**去掉 Bearer monkey-patch 只靠 cookie**(same-origin 本带 cookie);③ 或服务端 web 路径把优先级改 **cookie > header**。测试:租户 token 建项目 → `owner==web:<tenant>` 非 admin。

---

## 验收(草案)

- **F1**:`init --owner web:u1`→`state.owner=="web:u1"`;裸 `u1`→`web:u1`;re-init 带新 owner 覆盖(无 `--force`)、不带保留。
- **F2**:两租户各配自己 telegram/lark bot → 各自 `/pair` + 发消息 → 各只见/驱动自己会话;增一租户 bot → daemon 不重启热起监听;全局 bot 退役后 owner 经自己租户 bot 正常驱动。
- **F3**:`ccteam status` + admin 用户管理列出每租户登录链接;租户自助接口只回自己的、看不到别人。
- **F4**:tenant 建 session 面只见 claude/codex(固定 stream-json、默认无角色);admin 见全部(含 terminal/rmux + 角色)。beta-gating:admin 见 beta、tenant 不见(**纯 UI**,后端不变)。
- **Bug**:租户 web 建项目 → `owner==web:<tenant>`(非 admin)。
- baseline:cargo / ccteam-web / vitest 不退步;clippy 0;fmt clean。

## Dev-plan(波次,草案)

- **W1**:`ccteam init --owner`(小、独立、可先上)。
- **W2**:**修归属 bug**(SPA token/优先级)+ **F3 登录链接可见**(`/api/v1/users` 回 link + `ccteam status` + Settings UI)+ **F4 建-session 角色分级** + **「开发规范」beta-gating 机制**(`useMe().isAdmin` UI 门;写 `CLAUDE.md`)—— 多偏 SPA/小,解锁多租户可用性。
- **W3**:`Tenant` 加 IM 凭据 + 自助注册 REST(`/api/v1/me/im`)+ token 验证 + Settings UI。
- **W4**:transport 多 bot 监听 + per-tenant 路由/身份 + 热重载(**核心**);全局 bot 迁移/退役 + web↔IM `linked_chat`。
- **W5**:文档 + lark 对齐 + 收尾。

## 红线

- **No prompt injection / dual-SoT / session=一等实体** 不变。
- **soft-partition 诚实**:per-tenant bot = IM 路由+可见性隔离,**非** OS 安全边界(同 uid);真隔离 deferred。
- IM 凭据面:租户设自己的 bot 是**自助**;全局/admin 运维面仍 admin-gated(`deny_non_admin`)。登录链接对 **admin** 可见、租户间互不可见。

> **注:claude 上下文压缩(autoCompactWindow)不在本版** —— owner 2026-06-25 决定它**不做 ccteam 功能**,用户直接在本机 `~/.claude/settings.json` 写 `{"autoCompactWindow": N}`(ccteam spawn 用 `--setting-sources=user,project,local`,会读用户设置 → 生效)。

---

## Handoff(实现已落地)

**Decided(实现时定的工程决策):**
- **per-tenant channel key = `"<platform>@<tenant_id>"`**(`@` 分隔):唯一 channel-map key 让出站回信(`channels.get(reply_to.channel)`)落到**正确的 bot**,不串。这是 W4 能小改的关键 —— gateway 早已把 `reply_to`(每 turn 回信目标)与 `owner`(ACL key)**分开**,所以 channel 名随 `ChatKey` 自然流过,无需重写 361KB gateway。
- **ACL 按 platform**:`ThreeLayerSec` 对未知 platform fail-closed,故入站门用 `platform_of()` 剥掉 `@<tenant>` 后缀(`telegram@x`→`telegram`);回信仍用全名。
- **`chat_can_access` 隔离**:抽出纯函数 `chat_owner_visible(chat, owner)` 单测。租户 bot **只见自己**(不见共享 web 池、不见别租户 IM session);admin/全局 bot 保留「own + web 池」运维视图。
- **热重载按 changed-scope**:`reload_im_channels` 只重建变了的那一维(creds vs tenants),租户改动**不 blip owner 的活 bot**。web `PUT /me/im` 经 `AppState.gateway` 句柄 `request_im_reload()` 触发 → 租户 bot 即时起。
- **归属 bug 根因 = auth 优先级**(非建项目逻辑):改 `URL-shim → cookie → Bearer`(cookie=当前登录、Bearer=无 cookie 客户端回退)。
- **F4 默认无角色 + 纯 UI 门**:`useMe().isAdmin` 控制运行时/角色可见性,后端创建路由不变。

**Rejected / Deferred(需 owner 拍板是否后续做):**
- **web↔IM 收敛**:租户 bot 看自己的 **web-created** session —— 当前 gateway 把所有 web session 记 `web:web-api`(共享池,per-tenant 归属在 web REST/project 层,不在 gateway session.owner)。收敛要么让 gateway 按租户记 web session owner(动 v0.8.18 project-ownership 模型,风险大),要么 gateway 查 web-session→tenant。**本版做隔离不做收敛**。`Tenant.linked_chat` 字段留着、未接 per-tenant bot 身份。
- **全局 bot literal「废迁」**:owner 说「废+迁」「admin 也是一个有自己 bot 的租户」。实现为 **reframe**:全局 `credentials.json` bot 不再是共享 bot(租户各有自己的),它只服务 admin;**未**把 admin 塞进 tenants.json 当租户(避免迁移代码,守 pre-v1.0 不迁移规则)。功能上「租户不再用全局 bot」已达成。**若 owner 要 admin 也在注册表当租户,是后续小改。**

**Risks:**
- **soft-partition 诚实**:per-tenant bot = IM 路由+可见性隔离,**非** OS 安全边界(同 uid 仍可读他人 token/`/proc`)。真隔离 = per-user OS user/sandbox(deferred)。
- **同 token 不能两 bot**:每租户 bot token 必须 distinct(两 bot 同 token → `getUpdates` 409)。明文存 `tenants.json`(0600)。
- **bot 加载需 session 重启**无关(IM bot 是 daemon listener,非 claude session);但 daemon **重部署**后所有租户 bot 才按新代码起。

**Files(主要):**
- 核:`ccteam-core/src/tenants.rs`(IM creds + 0600 + setters)。
- 传输/网关:`ccteam-im/src/transport/{mod,providers/telegram,providers/lark}.rs`(`platform_of`/`is_tenant_bot_channel`/`with_name`/`self.name` stamp)、`ccteam-im/src/daemon.rs`(`build_tenant_channels`/路径/`build_channels`/`reload_im_channels`/ACL `platform_of`)、`ccteam-im/src/gateway.rs`(`chat_owner_visible`)。
- web:`ccteam-web/src/auth.rs`(优先级)、`routes/users.rs`(`/users/{id}/link`、`/me/im`、`/users/{id}/im`、热重载触发)、`routes/openapi.rs` + `tests/openapi_test.rs`(路由冻结表)。
- SPA:`ChatConsole.tsx`(F4 + Settings nav)、`SettingsPage.tsx`(MyImSection)、`lib/usersApi.ts`、`pages/chatDefaults` 不变(`resolveRole` 复用)。
- 规范:`CLAUDE.md §五.8`(beta-gating)。

**Remaining(本版后,按需):**
- web↔IM 收敛(见上)。
- admin 也进 tenants.json 当租户(若 owner 要 literal「admin=有 bot 的租户」)。
- lark 自助流的端到端真机验证(本版 lark 走 with_name + override,与 telegram 对称,但真机未跑)。
- `GET /me/im`(masked 回当前配置)让自助表单显示现状(当前 replace 语义 + 无 read → 表单从空起,留 UX 余地)。
