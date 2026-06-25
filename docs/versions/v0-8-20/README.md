# v0.8.20 — 多租户深化:CLI 项目归属 + per-tenant IM bot

> **Status: PLANNING / PRD**(doc-first,owner 2026-06-25 指示「先加到下个版本文档」)。**评审通过前不动代码。**
> 建立在**档1**(per-user web,v0.8.18)之上:`~/.ccteam/tenants.json` 租户注册表 + 单 token→`Identity{admin|tenant}` + **项目归属 ACL**(`ProjectState.owner`,web 建项目盖 `web:<tenant>`,session 继承 project 归属)。

---

## 背景 / 动机

档1 落地了 per-user **WEB**(每租户一个 web token + 身份 + 项目归属),但还有两个缺口:

1. **项目归属只能 web/IM 创建时获得** —— CLI `ccteam init` 建的项目 `owner=None`(admin/全局可见),没法把项目指给某个租户。
2. **IM 仍是单个全局 bot** —— `ccteam config` 配的那一个 telegram/lark bot,所有用户共用;web↔IM 复联 deferred(「tenant 当前 web-only,IM 接入需另走 bot allowlist」)。

本版把多租户从 web 推到这两处:CLI 能指定项目归属;每个用户用**自己的** IM bot(真正 per-user IM 隔离,顺带把 web↔IM 复联也解决 —— 租户的 bot 即其身份)。

**诚实范围(沿用档0/档1):** 单 OS-uid 下是**软隔离(UX/路由),非安全边界**(同 uid 仍可读他人文件/`/proc/<pid>/environ`)。per-tenant bot 隔离的是 IM **路由 + 可见性**(一个租户的 bot 只驱动它自己的会话),不是 OS 级隔离;真隔离 = per-user OS user / sandbox(仍 deferred)。

---

## Feature 1 — `ccteam init --owner <value>`(支持覆盖)

**目标:** CLI 起手时把项目直接指给某租户。

- 新增 flag:`ccteam init --owner <web:<tenant> | channel:<chat_id>>`,写入 `ProjectState.owner`。
- **归一化**:值含 `:` 按原样用;裸值(如租户 id `u1234abcd`)前缀补 `web:` → `web:u1234abcd`。空 → 视为未指定。
- **覆盖语义(owner 明确要)**:
  - 带 `--owner` → `owner = Some(归一化值)`,**覆盖**已有 owner(re-init 也覆盖,**不需要 `--force`**)。
  - 不带 `--owner` → **保留**现有 owner(新建 = `None`;re-init = 原值,绝不清零)。
- **轻校验**(不阻断):`web:<tenant>` 但租户不在 `TenantRegistry` → stderr WARN(「owner 不是已知租户,仍设置」),仍写入。
- init 回执显示 `owner: <value>` 让用户确认生效。
- **落点**:`ccteam init` argv(`main.rs` `Init` 变体)+ `InitOptions`(`commands.rs`)→ 透传到 `bootstrap_project_at_dir`/`bootstrap_project`(`projects.rs`,新增 `owner: Option<String>` 参,grep 全 caller 传 `None` 保持现状)→ 在写 `ProjectState` 处 set/override。
- 规模:小(1 flag + 透传 + override 逻辑 + 4 个用例测试)。

---

## Feature 2 — per-tenant IM bot(每用户自己的 IM bot)

**目标:** 每个租户跑**自己的** IM bot(自己的 telegram bot token / lark app),不再共用全局 bot;一个租户的 bot 只见/驱动它自己的会话。

**现状:** daemon 跑**一个**全局 telegram 监听(`getUpdates`)+ 全局 lark;inbound 按 `chat_id` 路由;凭据在 `config.yaml`(`ccteam config` 写),热重载走 `ccteam/reload`→`request_im_reload`(重建那一个监听)。

**设计:**
1. **租户 bot 凭据** —— 扩 `Tenant`(`tenants.json`)加 per-tenant IM 凭据:`telegram_bot_token: Option<String>`、`lark_app: Option<{app_id, app_secret}>`。**secrets**:`tenants.json` 在 `~/.ccteam`、gitignored;至少 `0600` 权限,考虑静态加密(开放决策)。
2. **多监听** —— daemon 为每个租户 bot 起**一个**监听(N 个 `getUpdates` 循环 / lark WS),复用现有 telegram/lark provider,按租户 token 参数化。transport 层从「单 bot」改成「按租户多 bot 实例」。
3. **inbound 路由 + 身份** —— 消息落在租户 X 的 bot 上 → 标 `Identity{tenant: X}`(复用档1 Identity)→ 路由到 X 的 current session;ACL/归属按 X。租户的 bot 只能见/驱动自己的会话。
4. **注册(租户设自己的 bot)** —— 自助:租户登录 web(自己的 token)在 **Settings → 我的 IM bot** 填 telegram bot token / lark app → 新 REST(如 `PUT /api/v1/me/im`);或 admin 经 `/api/v1/users/{id}` 代设。注册时 `getMe` 验 token,再存 + 起监听。
5. **热重载** —— 增改租户 bot → daemon 起/重启**那个租户**的监听(镜像现有 `ccteam/reload` 的 `request_im_reload`,从「重建全局」扩成「按租户增删监听」),无需重启 daemon。
6. **配对/allowlist** —— 每个租户 bot 自己的 allowlist(租户的 chat_id);`/pair` 按 bot 走。
7. **web↔IM 复联** —— per-tenant bot 天然解决:租户的 bot = 它的身份(tenant id),web 与 IM 两侧同一 `Identity{tenant}`。档1 留的 `linked_chat` 字段可落地。

---

## 开放决策(需 owner 拍板)

1. **全局 bot 的去留**:owner 说「不再使用全局的」。→ (A) 彻底废全局,所有 IM 都 per-tenant(admin 也是一个有自己 bot 的租户);(B) 全局只留给 admin/bootstrap。**倾向 A**:owner 现有全局 bot **迁移**成 owner 这个租户的 bot(同 token 不能两处用,必须迁移不是并存)。
2. **bot token 存储**:明文 `tenants.json`(0600)还是静态加密?
3. **注册权限**:租户自助设自己的 bot,还是只 admin 代设?(倾向自助 + admin 可代。)
4. **lark**:per-tenant lark app(app_id/secret)每租户配置更重 —— 本版做 telegram 优先、lark 跟上还是并列?
5. **资源**:N 个 `getUpdates` 循环(小 N 无压力);上限 / 懒启动?

---

## 验收(草案)

- `ccteam init --owner web:u1` → `state.owner == "web:u1"`;裸 `u1` 归一 `web:u1`;re-init 带新 owner 覆盖、不带保留;无 `--force` 也能覆盖 owner。
- 两个租户各配自己的 telegram bot → 各自 `/pair` + 发消息 → 各自只见/驱动自己的会话(交叉发不可见对方会话)。
- 增一个租户 bot → daemon 不重启就起监听(热重载)。
- 全局 bot 决策落地(废 or admin-only)后,owner 经自己的租户 bot 正常驱动。
- baseline:cargo / ccteam-web / vitest 不退步;clippy 0;fmt clean。

## Dev-plan(波次,草案)

- **W1**:`ccteam init --owner`(小,独立,可先上)。
- **W2**:`Tenant` 加 IM 凭据 + REST 注册(`/api/v1/me/im`)+ token 验证 + Settings UI。
- **W3**:transport 多 bot 监听 + per-tenant 路由/身份 + 热重载(核心)。
- **W4**:全局 bot 迁移/退役(按决策 1)+ web↔IM `linked_chat` 落地 + 文档。

## 红线

- **No prompt injection / dual-SoT / session=一等实体** 不变。
- **soft-partition 诚实**:per-tenant bot = IM 路由+可见性隔离,**非** OS 安全边界(同 uid);真隔离 deferred。
- IM 凭据面属**运维**:租户设自己的 bot 是自助;全局/admin 面仍 admin-gated(`deny_non_admin`)。

> **注:claude 上下文压缩(autoCompactWindow)不在本版** —— owner 2026-06-25 决定它**不做 ccteam 功能**,用户直接在本机 `~/.claude/settings.json` 写 `{"autoCompactWindow": N}`(ccteam spawn 用 `--setting-sources=user,project,local`,会读用户设置 → 生效)。
