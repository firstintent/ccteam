# v0.8.18 Handoff — Loop 地基(已落地)

> 状态:**已落地 dev**(无 tag,等 owner review)。PRD = [`README.md`](README.md);视觉 SoT = [`prototype/v0818-real-shell.html`](prototype/v0818-real-shell.html)。
> 实现姿势:在隔离 git worktree 里实现 + 分步提交推送 dev(Wave A→D),主仓不动(控制会话的 telegram/MCP 桥不掉)。**档1(per-user web token)按 PRD「选配」延后**到后续 patch。

## Decided(本版做了什么)

**柱1 · 控制台 + 主机**

- `GET /api/v1/status` 加 `sessions: Vec<SessionCostRow>` —— 每条 live session 的 best-effort 成本(读 `chat_turn_completed` 的 usage × `ccteam_cost::estimate_cost`;model 未逐回合记 → fallback 定价,诚实标注);SPA `StatusView` 会话行加成本列。**舰队骨架**,下版在同一批行加 loop 列。
- `GET /api/v1/hosts`(+ `/{host}` 详情 + `POST …/register-mcp` 唯一可写、幂等)= host-keyed agent 报告(hostname/os/arch/ccteam version + 每 vendor 装/`--version`/MCP 注册/就绪态)。`capabilities.rs` 收敛到同一个 `hosts::probe_bin`(版本捕获 + 路径缓存 + `?refresh` 重探;`PROBE_SPECS` vendor-可扩展)。MCP 注册逻辑抽进 **`ccteam_core::mcp_register`**(`install_*_into` + `*_mcp_registered` 检测 + codex 路径解析;CLI `mcp_serve` 改为薄委托)+ **`ccteam_core::host::read_hostname`**。SPA 新增 `HostsView` + `lib/hostsApi.ts`,挂进 ChatConsole 壳。

**柱2 · 多用户软分区档0**

- `chat_can_access` 收 **own-only**(`session.owner == *chat`):删「web-channel 通看」+「同-current-project 互看」两漏(后者**反转 v0.8.13 跨前端按项目共享**);`/use`/`/stop`/`/screen`/`render_sessions` 全过它(`/use` 之前**没有** owner 门,本版补上)。
- `ProjectState.owner: Option<String>`(serde-default,旧 state.json 照载;`ChatKey::identity()` → `channel:chat_id`;`create_project`(IM `/newproject`)时记)。**显式字段、非路径派生**。
- **诚实:同 OS uid 软隔离(UX)、非安全边界。**

**UI 一致性**

- 界面语言 **中文(默认)/ English**:`useWebSettings` 加 `language/displayName/avatar`(+ SSR-safe server snapshot)+ `lib/i18n.ts`(`tr`/`navLabel`);ChatConsole 导航 + 面包屑随语言。
- 点头像 → 个人设置弹层(`components/AvatarMenu.tsx` + 纯 `AvatarPopover`,SSR-可测):显示名 / 头像 swatch / 语言 / 登出。
- 全局 Settings 加多用户档0 段(诚实说明边界 + 指向档1 的 `ccteam user add`)。

## Rejected / Deferred

- **档1(per-user web token + 用户注册表 + `ccteam user` CLI)**:PRD 标「选配/不阻塞本版」,延后到后续 patch(半成品多租户比干净延后更糟)。Settings 段 + ACL 注释已指向它。
- 全量 UI i18n(每条字符串翻译):本版只 nav + 关键 label;全量是独立小版本。
- 真·分布式 host 调度:本版 host 只「列」(一台 `local`),起跑 / 路由留后。
- loop 运维台的「预言机 / 门」列、on-ramp、oracle-diff 门:loop 本身是下一版(本版只是地基)。

## Risks / 行为变化(owner 须知)

- **跨前端共享反转**:同一用户的 web 会话与 IM 会话不再自动互见(各 `chat_id` 隔离),直到档1 用共享身份打通。这是有意的多用户隔离;CLAUDE.md §三 新增 own-only 红线、勿再加回。
- per-session 成本是 best-effort(fallback 定价 + 仅 stream-json 会话写 usage);tmux 会话显示 0.00。下版可正式化(把 vendor `total_cost_usd` 接到 gateway 累加器)。

## Files(主要)

- core:`src/host.rs`(新)· `src/mcp_register.rs`(新)· `src/state.rs`(owner 字段)
- cli:`src/mcp_serve.rs`(委托核到 core)· `src/web_chat_bridge.rs`(own-only 测试)
- im:`src/gateway.rs`(own-only ACL + `ChatKey::identity` + `create_project` owner)
- web:`src/routes/{hosts.rs(新),capabilities.rs,status.rs,openapi.rs,mod.rs}` + `tests/openapi_test.rs`
- SPA:`pages/HostsView.tsx`(新)· `components/AvatarMenu.tsx`(新)· `lib/{hostsApi,i18n}.ts`(新)· `pages/{ChatConsole,StatusView,SettingsPage}.tsx` · `hooks/useWebSettings.ts` · `lib/{statusApi,token}.ts` · `App.tsx`

## 验收结果(全绿)

- `cargo test --workspace --exclude ccteam-web`:**2031 / 0**(baseline 2016)
- `ccteam-web`:**285 / 0**;vitest **182 / 0**;tsc + eslint clean
- `cargo clippy --workspace --all-targets -- -D warnings`:0;`cargo fmt --all -- --check`:clean
- `GET /api/v1/hosts`:host=`local` + claude=ready(带 version)/ codex=not_installed;`register-mcp` 幂等(确定性 fake `CCTEAM_*_BIN`)
- ACL:两个 `chat_id` 互不可见 / 不可 `/use`(确定性假 `chat_id`)

## 上线还需(owner)

- daemon **重部署**(0.8.18 binary)+ SPA **重 build**(主机页 / 语言 / 成本列要新 bundle)+ 用户 `/mcp` 重连。
- **tag 仍 HOLD** —— 等 owner review。
