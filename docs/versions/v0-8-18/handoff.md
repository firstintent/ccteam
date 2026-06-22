# v0.8.18 Handoff — Loop 地基(已落地)

> 状态:**已落地 dev**(无 tag,等 owner review)。PRD = [`README.md`](README.md);视觉 SoT = [`prototype/v0818-real-shell.html`](prototype/v0818-real-shell.html)。
> 实现姿势:在隔离 git worktree 里实现 + 分步提交推送 dev(Wave A→D + Wave E),主仓不动(控制会话的 telegram/MCP 桥不掉)。**档1(per-user web,web-first)已作本版后续增量落地**(Wave E,owner 拍板「统一到 web 写入、CLI 只 bootstrap」);web↔IM 同一人复联仍延后。

## Decided(本版做了什么)

**柱1 · 控制台 + 主机**

- `GET /api/v1/status` 加 `sessions: Vec<SessionCostRow>` —— 每条 live session 的 best-effort 成本(读 `chat_turn_completed` 的 usage × `ccteam_cost::estimate_cost`;model 未逐回合记 → fallback 定价,诚实标注);SPA `StatusView` 会话行加成本列。**舰队骨架**,下版在同一批行加 loop 列。
- `GET /api/v1/hosts`(+ `/{host}` 详情 + `POST …/register-mcp` 唯一可写、幂等)= host-keyed agent 报告(hostname/os/arch/ccteam version + 每 vendor 装/`--version`/MCP 注册/就绪态)。`capabilities.rs` 收敛到同一个 `hosts::probe_bin`(版本捕获 + 路径缓存 + `?refresh` 重探;`PROBE_SPECS` vendor-可扩展)。MCP 注册逻辑抽进 **`ccteam_core::mcp_register`**(`install_*_into` + `*_mcp_registered` 检测 + codex 路径解析;CLI `mcp_serve` 改为薄委托)+ **`ccteam_core::host::read_hostname`**。SPA 新增 `HostsView` + `lib/hostsApi.ts`,挂进 ChatConsole 壳。

**柱2 · 多用户软分区档0**

- `chat_can_access` = **own + 共享 web 池**(`session.owner == *chat || session.owner.channel == "web"`):一个 chat 见/驱动自己的 session + 所有 web 控制台创建的 session(web 是单一共享操作台,到档1 才 per-user;支撑「web 建、手机驱动」单用户流),**IM 之间各自建的 session 互相隔离**。删「同-current-project 互看」(**反转 v0.8.13 跨前端按项目共享**);`/use`/`/stop`/`/screen`/`render_sessions` 全过它(`/use` 之前**没有** owner 门,本版补上)。<br>**注**:初版 own-only 把 web 也隔离了,导致 owner 在 web 建的 session 在 TG 看不到(部署后实测发现)→ 已修成 own + web 池。
- `ProjectState.owner: Option<String>`(serde-default,旧 state.json 照载;`ChatKey::identity()` → `channel:chat_id`;`create_project`(IM `/newproject`)时记)。**显式字段、非路径派生**。
- **诚实:同 OS uid 软隔离(UX)、非安全边界。**

**柱2b · 多用户档1(per-user web,web-first 后续增量 / Wave E)**

- `~/.ccteam/tenants.json` 租户注册表(`ccteam_core::tenants`:`Tenant{id,handle,web_token,linked_chat?,created_at}` + 原子存盘 + `by_token` 常量时比较)。
- web auth 从单 token 升成 **`token→Identity{admin|tenant}`**:admin=bootstrap owner token、tenant=注册表 token;`auth_layer` 注入 `Extension<Identity>`(no-auth/loopback 也注入 admin)。
- REST `GET/POST /api/v1/users` + `DELETE …/{id}`(**admin-gated**;POST 回一次性 personal link `?token=ccteam:<hex>`,GET 永不回 token)。
- **项目归属 ACL**(owner 拍板「项目=归属单元、会话属于项目→属于用户」;改掉初版 per-session owner = 撤 `session_views_for_web`/`web_owner_chat`/`create_session_api_proto(web_owner)`):web 建项目盖 `ProjectState.owner=web:<tenant>`;**session 继承其 project**(`session.owner` 只留回信路由)。web REST 全按 `can_see_project(identity,slug)` 鉴权 —— project 列表/详情(404)/创建 + 该 project 下 session 列表/创建/按-sid(`gate_sid`:history/status/turn/resolve/events/stop)。**IM `chat_can_access` 不动**。
- **全局·运维面 admin-gated**(`auth::deny_non_admin`→403):IM 凭据 `config/im*`、主机 `hosts`+`register-mcp`、`status`、用户 `users`。`GET /api/v1/me` → SPA 按身份隐藏 Status/主机/Settings nav + IM 凭据段(fail-closed `useMe`/`meApi`)。共享核 `Identity::{web_owner,can_see_owner}`。
- Settings **用户管理 UI**(`lib/usersApi.ts`:添加/列/删 + 个人链接复制;403→只读提示)。
- **无 `ccteam user` CLI**(owner 决策:runtime 写归 web/IM/REST,CLI 只 bootstrap)。

**UI 一致性**

- 界面语言 **中文(默认)/ English**:`useWebSettings` 加 `language/displayName/avatar`(+ SSR-safe server snapshot)+ `lib/i18n.ts`(`tr`/`navLabel`);ChatConsole 导航 + 面包屑随语言。
- 点头像 → 个人设置弹层(`components/AvatarMenu.tsx` + 纯 `AvatarPopover`,SSR-可测):显示名 / 头像 swatch / 语言 / 登出。
- 全局 Settings 加**用户管理 UI**(档1,web-first:添加/列/删用户 + 一次性个人链接复制;`lib/usersApi.ts`)。

## Rejected / Deferred

- **web↔IM 同一人复联**(`linked_chat`:tenant 的 web 身份 ↔ 其 IM chat):注册表字段 + 方法已留,但 tenant 当前 **web-only**(IM 接入要另走 bot allowlist)→ 延后。**`ccteam user` CLI** 不做(owner 决策:runtime 写归 web/IM/REST,CLI 只 bootstrap)。
- 全量 UI i18n(每条字符串翻译):本版只 nav + 关键 label;全量是独立小版本。
- 真·分布式 host 调度:本版 host 只「列」(一台 `local`),起跑 / 路由留后。
- loop 运维台的「预言机 / 门」列、on-ramp、oracle-diff 门:loop 本身是下一版(本版只是地基)。

## Risks / 行为变化(owner 须知)

- **共享模型**:web 控制台是**单一共享操作台池** —— 所有 chat(含 IM)都能看/驱动 web 创建的 session(支撑「web 建、手机驱动」单用户流);但 **IM 之间各自建的 session 互相隔离**。同-current-project 互看(v0.8.13)被反转、勿加回。档1(per-user web token)让 web 池也 per-user。CLAUDE.md §三 有红线。
- per-session 成本是 best-effort(fallback 定价 + 仅 stream-json 会话写 usage);tmux 会话显示 0.00。下版可正式化(把 vendor `total_cost_usd` 接到 gateway 累加器)。

## Files(主要)

- core:`src/host.rs`(新)· `src/mcp_register.rs`(新)· **`src/tenants.rs`(新,档1 注册表)** · `src/state.rs`(owner 字段)· `src/paths.rs`(`tenants_json`)
- cli:`src/mcp_serve.rs`(委托核到 core)· `src/web_chat_bridge.rs`(ACL 测试)
- im:`src/gateway.rs`(ACL + `ChatKey::identity` + `create_project` owner; 档1 项目归属模型 = web session 复用 `web_api_chat` 池,ACL 移到 web 层)
- web 档1 隔离:`src/auth.rs`(`Identity::{web_owner,can_see_owner}` + `deny_non_admin`)· `routes/api_v1.rs`(`can_see_project` + `handle_me` + project 过滤)· `routes/sessions_api.rs`(`gate_sid` + project 门)· `routes/{im_config,hosts,status}.rs`(admin 门)· SPA `lib/meApi.ts` + `hooks/useMe.ts` + `ChatConsole`/`SettingsPage`(按身份隐藏)
- web:**`src/auth.rs`(档1 `Identity`/`resolve_identity`,`auth_layer` 注入)** · `src/routes/{hosts.rs(新),users.rs(新,档1),sessions_api.rs(Identity scope),capabilities,status,openapi,mod}` + `tests/openapi_test.rs`
- SPA:`pages/HostsView.tsx`(新)· `components/AvatarMenu.tsx`(新)· `lib/{hostsApi,i18n}.ts`(新)· **`lib/usersApi.ts`(新,档1)** · `pages/{ChatConsole,StatusView,SettingsPage(用户管理)}.tsx` · `hooks/useWebSettings.ts` · `lib/{statusApi,token}.ts` · `App.tsx`

## 验收结果(全绿)

- `cargo test --workspace --exclude ccteam-web`:**2039 / 0**(baseline 2016)
- `ccteam-web`:**292 / 0**;vitest **188 / 0**;tsc + eslint + vite build clean
- 档1:`resolve_identity` · `deny_non_admin` · `can_see_owner`(admin/tenant) · `TenantView`-不漏-token · **`tenant_acl_test`**(端到端:tenant token → im_config/hosts/status 403 + users 403 + projects [] + `/me` 身份)· `usersApi`/`meApi`(401/403/500 映射)
- `cargo clippy --workspace --all-targets -- -D warnings`:0;`cargo fmt --all -- --check`:clean
- `GET /api/v1/hosts`:host=`local` + claude=ready(带 version)/ codex=not_installed;`register-mcp` 幂等(确定性 fake `CCTEAM_*_BIN`)
- ACL:两个 `chat_id` 互不可见 / 不可 `/use`(确定性假 `chat_id`)

## 上线还需(owner)

- daemon **重部署**(0.8.18 binary,含档1 注册表 + roleless `/new`)+ SPA **重 build**(主机页 / 语言 / 成本列 / 用户管理要新 bundle)+ 用户 `/mcp` 重连。
- **首个 web 用户**:owner 用 bootstrap web token 进 Settings → 用户管理 添加 → 复制个人链接发给对方;对方打开即以自己身份登录,只看自己的 session。
- **tag 仍 HOLD** —— 等 owner review。
