# v0.8.8 Phase 3 handoff — web 功能补全(F4 config / F5 role 页 / F2-web)+ UI 质量基线

> commit `ea6122b`(已 push origin/dev)。Gate:`cargo test --workspace --exclude ccteam-web` **1997/0** · clippy --workspace --all-targets **0** · fmt 干净 · ccteam-web **224 pass + 4 env-gated `ws_*`**(tmux pipe-pane,沙箱不可流)· SPA vitest **151/151**(基线 120,+31)。
> 编排:recon-only workflow(3 路并行 opus)→ 主控审 + 拍 nav/异步 chat_id 机制 → 前台 opus agent ×3(3A F4 后端 / 3B F4 前端 / 3C F5+F2-web)→ 主控权威 gate + 对抗 review **兼** PRD §二 UX 走查(verdict=pass,三页 UX 全过)。

## Decided(已定 + 落地)
- **F4 web config(/api/v1/config/im,web-token 门后)**:GET masked 状态(响应类型**根本不含** bot_token/app_secret 字段 → 类型层杜绝明文回显,只 *_last4+counts+use_feishu)· PUT telegram/lark(先 getMe / tenant_access_token **验过再落盘** 0600)· telegram chat_id **异步**(POST start 起背景 long-poll task(90s 预算)+ AppState.im_poll;GET 轮询状态;captured → dedup 进 allowed_chat_ids)· 所有写返回 **restart_required**(creds 仅 daemon 启动时 load 一次,无 hot-reload)。复用 onboarding(把 telegram_setup 拆出 validate-token + poll-chat-id 两个 pub fn)。
- **F4 前端 SettingsPage(Settings tab)**:四态;token/secret input 永 password + **不预填**已存值;覆盖已配 secret = 内联两态确认(非 window.confirm);lark 空 allowlist fail-closed 警示;chat_id 轮询可取消(useRef+cleanup,终态停);LAN 明文警示;surface-*/brand-*/status-* token(无裸色)。
- **F5 web role 浏览(Roles tab,只读)**:RolesPage —— 选项目 → 列 roles(已有 API)→ 详情(新 getRoleDetail;frontmatter key/value,非标量 JSON 兜底;body marked + .cockpit-markdown)。三层各四态 + 错误人话 + 重试。后端已具备(纯前端)。
- **F2-web**:NewSessionModal 加 "(无角色 / 裸 claude)" 选项,用 ROLELESS sentinel + resolveRole(显式选 roleless → "" **不** fallback cto;未选/空 → DEFAULT_ROLE,**守 FIX-2**);后端 sessions_api 删空-role 400(gateway F2 已接受空 role,不触 422)。
- **nav 决策(PRD F5 开放问题)**:F5 = 独立 Roles tab(浏览,跟 Projects/Sessions 并列);F4 = 独立 Settings tab(配置)。两个新 tab(共 6),短屏可接受。
- **异步 chat_id 决策**:背景 task long-poll(单 getUpdates session,offset 正确)+ 前端轮询 cheap status(可取消),非阻塞请求线程。
- **测试隔离**:F4 后端测试用 tempdir creds_path(`with_creds_path`)+ env mock seam(`CCTEAM_TELEGRAM_API_BASE`/`CCTEAM_LARK_API_BASE`),**绝不写真实 ~/.ccteam**;env-mutating 测试 #[serial]。

## Rejected(否决 + 因由)
- **F4/F5 合并进单一 "Settings/角色" 导航区**:否(各独立 tab,F5 是浏览、F4 是配置,语义不同)。
- **creds 热生效**:做不到(daemon 启动时 load 一次,无 reload/watch)→ PUT 返 restart_required + UI 提示重启。
- **catalog 在线浏览/装(GET /catalog/roles)**:否(F5 只读已装;catalog = stretch,本版不做)。
- **window.confirm 危险确认**:否(内联两态按钮,与暗色主题一致)。
- **F2-web 删 fallback 不加 sentinel**:否(会把默认 cto 也变 roleless,破 FIX-2)→ 用显式 ROLELESS sentinel。

## Risks(残留 + 监控)
- **4 个 `ws_*` env-gated**(tmux pipe-pane,沙箱不可流)—— 长期既有环境失败,不计基线,留 CI/专机。
- **chat_id 重复 start**:连续 start(token 改/重试)会起新 task;旧 in-flight task 可能后写 slot。实践无重叠(start 只从 save / timeout-retry 触发),低危,未做硬互斥(可后续加)。
- **rolesView humanError**:NOT_FOUND 文案改为 "项目或角色不存在(可能已删除)"(覆盖 role-deleted-between-list-and-click race;原 "项目不存在" 不准 —— 已修)。
- **ChatConsole 裸色(amber-*/red-*)**:pre-existing(v0.8.7 W4),非 Phase 3 引入;Phase 3 只动 sentinel/preview(用 token)。统一 ChatConsole 配色 = 后续 chore(非阻断)。
- **vitest 无 jsdom**:SettingsPage/RolesPage 交互态(轮询/点详情)renderToString 测不到 → 逻辑抽进 configApi/纯 helper(可测),全链路留 playwright(host E2E,非基线)。

## Files(改了什么)
- F4 后端:crates/ccteam-im/src/onboarding.rs(telegram split)· crates/ccteam-web/src/routes/im_config.rs(新,5 handler + masked struct + mask_last4)· routes/{mod,openapi}.rs(注册)· state.rs(im_poll + creds_path test seam)· tests/im_config_test.rs(新,12)+ openapi_test(+5 ops)。
- F4 前端:web/src/lib/configApi.ts(新)· pages/SettingsPage.tsx(新)· App.tsx(Settings tab)· lib/configApi.test.ts + pages/SettingsPage.test.tsx(新)。
- F5:web/src/lib/sessionsApi.ts(getRoleDetail + RoleDetail)· pages/RolesPage.tsx + rolesView.ts(新)· App.tsx(Roles tab)· 测试。
- F2-web:web/src/pages/chatDefaults.ts(ROLELESS + resolveRole)· ChatConsole.tsx(sentinel + preview)· crates/ccteam-web/src/routes/sessions_api.rs(删空-role 400 + utoipa)· 测试。

## Remaining(Phase 4 + ship gate)
- **Phase 4(最后)**:① 重写红线文档 —— CLAUDE.md §〇/§三(session = 一等实体 + 持久 sid;role 是属性;去 dedup;resume-by-sid)+ §一 baseline 回填 + §四(skills/ 已删的 doc-drift,Phase 0 review low)+ tech-design.md(架构 SoT + 协议→代码指针表,补 /api/v1/config/im* + roles 页 + session=sid)② 用户面 README.md(英文、不夹版本进展)+ docs/usage.md(融入独立 session / web config / role 页 / status 新格式 / roleless)③ 版本 workspace Cargo.toml 0.8.7→0.8.8 ④ 归档 docs/versions/v0-8-8/README.md(冻结里程碑)⑤ 收尾 gate(full cargo test ≥1997/0 + clippy 0 + fmt + vitest + `doctor --verify-mcp` no drift + skill-gate 仍过(skills/ 已删))。
- **ship gate 前(专机)**:real-claude /role `--name`-collision smoke(#[ignore],Phase 4 加 test 占位)+ W2 PermissionRequest 在 multiple same-role 下复跑。
- **dead-chain cleanup(非阻断,可 v0.8.9)**:删 supervisor/outbound/BotSupervisor + chat_history/send_input 死工具。
- **HOLD git tag**:收尾全绿后 push,但 tag 留给 user sign-off([[ship-flow]])。
