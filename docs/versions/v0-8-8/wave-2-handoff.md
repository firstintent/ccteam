# v0.8.8 Phase 2 handoff — 建立在 F1 上(B4 / F3 / B5 / F2 / B3-verify)

> commit `2b84e69`(已 push origin/dev)。Gate:`cargo test --workspace --exclude ccteam-web` **1997/0**(F1 基线 1984,+13 净新)· clippy --workspace --all-targets **0** · fmt 干净 · ccteam-web 209 pass + 4 env-gated `ws_*`(tmux pipe-pane,项目级路由,未动)· SPA vitest **120/120**。
> 编排:recon-only workflow(5 路并行 opus,读 F1 之后的代码)→ 主控审 + 拍取数机制 → 前台 opus agent ×3(2A B4+F3 / 2B B5 / 2C F2)+ B3-verify(纯验证)→ 主控权威 gate + 对抗 review(verdict=pass)。

## Decided(已定 + 落地)
- **B4/F3 共用取数 = 持久化 gateway state 文件 reader**(`tracked_chat_sessions()`,gateway.rs;**不**新增 daemon RPC、**不**碰 cto-gate)。两个 recon agent 给了两套(新 sessions/snapshot RPC vs 读持久化文件),主控选**读持久化文件**:已含 vendor+sid+role+permission_mode、零新 socket 面、sub-second persist-drift 对 status/ls 可接受。
- **B4 / BUG-5**:`session ls` 行源改 tracked_chat_sessions;tracked 行(含 codex)status=live(daemon up)→ **修掉 codex 误报 "registered, not running"**;加 VENDOR 列(+ SID 列);orphan = live pane ∧ 不在 tracked;daemon-down 降级 "registered (daemon down)" 不报错。
- **F3 / status 重写**:嵌套列所有 project + 各自 sessions(role 空→`-`/vendor/status/sid/last-event);删 recent events 段 + 删 `--tail` arg;两行 web token(裸 hex)/ url(带 `ccteam:` 前缀 + LAN ip);LAN ip = `libc::getifaddrs` 第一个非 loopback/link-local 私网 IPv4(零新 dep);保留项目级 STUCK/OK。
- **B5 / BUG-6**:`handle_session_ws` 经 `gateway.session_resolve(sid)` 解析 per-session pane(vendor 分支:claude=`chat_session_name`/codex=`codex_chat_session_name`),共享 `resolve_session_pane` helper(pty_ws + pane_snapshot 共用);no-gateway→503、unknown sid→404;`send_keys`/`resize` 改 `default_backend()`;`codex_chat_session_name` 抽为单一权威。终端 UI 保持 claude-only。
- **F2 / roleless(后端)**:空 role → spawn 不加 `--agent`(brain 走项目 CLAUDE.md);`spec_for_new/resume` 仅非空 role push `--agent`;删 start_thread 非空-role 硬挡;`ensure_role_exists` 空-role 短路(在 read_role 前,守 FIX-2 非空);handle 空 role → 回退 sid。
- **B3-verify**:BUG-3 由 F1 结构性根治已确认(读+写均按 sid,4 验证点过,既有测试覆盖);无需新代码。

## Rejected(否决 + 因由)
- **B4 用新 daemon `sessions/snapshot` RPC**:否(选持久化文件 reader)—— 避免新 socket 面 + 不碰 cto-gate;drift 可接受。
- **F2 本轮开 web roleless**(放宽 sessions_api 400 + NewSessionModal "无角色" 选项):否 → 留 **Phase 3**(与其余 web 活一起);本轮 roleless 入口 = create_session_api(cto session_spawn / 编程)。
- **删 chat_history 死工具 / 删死 supervisor 链**:否(本轮不碰,留 dead-chain cleanup follow-up;F1 已 TODO 标注,优雅返回空)。
- **改 IM /new 缺省为空 role**:否(裸 /new 仍 cto,不意外变 roleless)。
- **B5 放开 codex 终端 UI**:否(canTerminal 仍 claude-only;后端 codex pane 已支持,真机验后再放开,UI 不承诺未验能力)。
- **保真度假装裸字节做完**:否(诚实降级:rmux 行文本 / tmux 裸字节 + TODO)。

## Risks(残留 + 监控)
- **B4/F3 live-vs-persisted drift**:CLI 读持久化 state(非 in-mem session_views),daemon 刚 spawn/stop 未落盘时短暂不一致;status 列用 live-pane 集合算 orphan 缓解;daemon-down 标 "(daemon down)"。
- **per-session last-event 缺失**:gateway state 无 per-session 时间戳 → 会话行 last-event 挂项目级 stall(同项目多 session 同值)或 `-`(注释说明)。
- **web url port 硬编码 7331**:无 live 查询拿不到真实 `--web-bind` 端口(符 user 实例,已注释局限)。
- **LAN ip 多网卡非确定**:`getifaddrs` 取首个私网 IPv4(docker0/wifi/vpn 序不定);unsafe FFI 已包单 fn + freeifaddrs + AF_INET 判定 + 字节序,review 过内存安全。
- **4 个 `ws_*` + 真 per-session 字节中继 env-gated**:沙箱起不了 tmux pipe-pane / 需 live gateway,留 CI/专机;B5 把原项目级-fallback 测试改成 no-gateway→503(沙箱可确定测),真 pane 字节留 env-gated(诚实标注)。
- **stale 测试教训**:F2 删 start_thread 非空-role 守门,漏了 `ccteam-core/tests/harness_trait_test.rs::claude_tui_rejects_empty_role`(在 ccteam-core 测 harness trait,2C 只跑了 harness+im)→ workspace gate fail-fast 抓出。已删该 stale 测试(行为按设计移除,roleless argv 由单测覆盖)。**教训:改 harness 公共行为要 grep 全 workspace 测试(含 ccteam-core/tests),per-crate test 跑不全。**

## Files(改了什么)
- im:gateway.rs(`tracked_chat_sessions` + `TrackedSessionRow` reader · F2 ensure_role_exists 空短路 + handle→sid 回退)。
- cli:commands.rs(`run_sessions` 用 reader + VENDOR 列 + `render_sessions_table` 抽测)、main.rs(`run_status` 重写 + `first_lan_ipv4`/`is_lan_ipv4` getifaddrs + 删 --tail)、tests/status_view_test.rs(新)。
- harness:claude_tui.rs(spec_for_new/resume roleless 条件 --agent + 删 start_thread guard)、codex_exec.rs(`codex_chat_session_name` 抽取)、lib.rs(re-export)。
- web:routes/session_pane.rs(新,共享 sid→pane helper)、pty_ws.rs(sid 解析 + default_backend + doc)、pane_snapshot.rs(同 helper)、ChatConsole.tsx(canTerminal claude-only + TODO)。
- 测试:删 1 stale(claude_tui_rejects_empty_role)+ 新增 B4/F3/B5/F2 各项单测/集成测;B5 把 env-flaky 的 pane_snapshot/pty_ws 测试改 no-gateway→503 确定性。

## Remaining(留给后续阶段)
- **Phase 3**:F4(web config:telegram+lark,秘密 mask、chat_id 异步、无 TLS)· F5(web role 浏览页,只读已装)· **F2 web 收口**(放宽 sessions_api 空-role 400 + NewSessionModal "无角色" 选项)· 全程守 UI 质量基线 + UX review。
- **Phase 4**:文档(CLAUDE.md §〇/§三 + tech-design 重写 session=一等实体/role 属性/resume-by-sid + 协议指针表;README/usage 融入新能力)+ 版本 0.8.7→0.8.8 + CLAUDE.md §一 baseline 回填 + skills/ doc-drift 修(Phase 0 review 的 low)+ 归档 README + 收尾 gate。
- **收尾前(ship gate)**:real-claude /role `--name`-collision smoke(#[ignore])+ W2 PermissionRequest 在 multiple same-role 下复跑。
- **dead-chain cleanup(非阻断,可 v0.8.9)**:删 supervisor/outbound/BotSupervisor 链 + chat_history/send_input 死工具(F1 已 TODO 标)。
