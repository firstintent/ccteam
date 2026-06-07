# v0.8.8 Phase 1 handoff — F1 独立 session 模型(keystone)

> commit `529768d`(已 push origin/dev)。Gate:`cargo test --workspace --exclude ccteam-web` **1984/0**(基线 1977,+7 净新)· clippy --workspace --all-targets **0** · fmt 干净 · ccteam-web F1 测试绿(internal_hook 7/7、sessions_api collect 3/3、lib 109)。
> 编排:recon-only workflow(6 路并行 opus + 1 架构综合 → 审定的 11-step plan,存 /tmp/v088-f1-plan-slice.txt)→ 主控亲审方向 → impl workflow(G1 落盘后因 host suspend 停摆)→ 主控用前台 opus agent 续 G2/G3/G4 + 对抗 review(verdict=pass)。

## Decided(已定 + 落地)
- **session = 一等实体 + 持久 sid**:`s<N>` 单调、持久化进 state、扛 daemon 重启、不复用(next_session 持久化既有,acceptance 验证)。role 降为属性。**结构性根治 BUG-3**。
- **pane/turns/marker 全按 sid**:`chat_session_name(slug, sid)`、pane=`ccteam-chat-<slug>-<sid>`、turns=`.ccteam/chat/<sid>/turns.jsonl`(turns_mirror + transcript_tail + session_recovery)。`CCTEAM_CHAT_SID` 注入 pane env(`chat_spawn_env_owned`)+ daemon HTTP 路径加 `X-Ccteam-Sid` header → hook(chat_progress marker)与 in-pane forwarder 都拿得到 sid;写/读 marker 同键(无 split-brain 静默)。
- **【命门修正】gateway 补 turns writer**:`spawn_event_pump` 的 ANSWER 分支现 `append_turn(by session.id)`。**改前 live daemon 根本不写 turns.jsonl**(唯一 writer BotSupervisor 是死代码、daemon 未 wire)—— 只修读侧会让历史永远空 + FakeAdapter 测试假绿。加了 `event_pump_writes_turns` regression 兜住。
- **去 dedup**:删 start_session 的 (project,role) reuse + load_state collapse → 同 role 多 session 并存;/new 总铸新 sid;普通消息复用走 `ensure_current_session`(真正的 storm guard,未动)。
- **HITL/cto sid 身份**:permission/ask 转发 **ccteam sid**(绝不用 Anthropic session UUID);session_sid_for/reply_target_for 改 sid;verify_session_caller 保持 (role,secret) 配对未弱化。SessionResolve 加 vendor。
- **3 个开放问题拍板**:① /role = carry-context(同 sid+pane 原地 re-spawn)+ fresh-spawn 分支加 death-probe(失败不误报成功);② 死 supervisor 链(chat_history/send_input 的 role-keyed inbox/turns)F1 不删,TODO 标注 + 优雅返回空;③ IM home-chat 绑定本轮仍 (slug,role)。

## Rejected(否决 + 因由)
- **/role 用 role-epoch 名后缀(B 方案)**:否。保持 pane=sid 纯净;carry-context + death-probe 足够,真机语义留 smoke 验。
- **F1 内删死 supervisor/outbound 链**:否(会污染 keystone diff)→ 后续 cleanup。
- **per-sid IM 路由**:否(要动 chat_register_bot/BotRegistration/web SSE 回址,blast radius 翻倍,F1 验收不需要)→ deferred。
- **TurnRecord 加 session_id 字段**:否(目录 `.ccteam/chat/<sid>/` 已隔离,读永远经 session_resolve(sid)→project_dir→read_all_turns(sid))。

## Risks(残留 + 监控)
- **/role 真机 --name-collision 行为未验**:F1 下 /role 同 sid → --name 不变,而旧 role 的 claude jsonl 仍在该 name 下。fake bin 复现不了 --name 碰撞。**收尾前需一条 #[ignore] real-claude smoke** 确认 carry-context 行为(resume 旧线 vs 报错)+ death-probe 真拦得住。已在 plan/handoff 记录。
- **2 个 pane_snapshot 测试 env-fail**:`pane_snapshot_uses_state_tmux_session_for_meta_projects` + `..._session_route_falls_back_to_project_tmux_session` 因 sandbox 起不了 rmux daemon(5s timeout)而红。**已在 a6051cb(pre-v0.8.8)实证同样红 → 是 pre-existing 环境失败,非 F1 回归**,与 5 个 ws_* 同 carve-out(ccteam-web env-gated,不计入 --exclude ccteam-web 基线)。
- **死链 chat_history/send_input**:role-keyed 路径已废(turns 现按 sid 写),TODO 标注 + 优雅空。chat_history 真正能用要等 registry 带 sid。
- **workflow 在 host suspend 下停摆**:F1 impl workflow 跑完 G1 后遇 8h host 挂起,runner 进程死、G2-G4 没起。教训:长 workflow 抗不住 host suspend;主控用前台 opus agent 续跑更稳(已用此法收尾)。

## Files(改了什么,~23 文件 / 4 crate)
- harness:claude_tui.rs(sid env + pane/name re-key + spec_for_* sid 参 + death-probe)、turns_mirror.rs / transcript_tail.rs / session_recovery.rs(bot_role→sid)。
- hooks:chat_progress.rs(derive_sid + marker by sid)、permission_request.rs(forward session_sid)、intercept_ask.rs。
- im:gateway.rs(turns writer + 去 dedup + load_state + SessionResolve.vendor + session_sid_for/reply_target_for→sid + reconcile/orphan→sid)、daemon.rs(orphan sid 展示)。
- cli:main.rs(execute_permission_ask/interaction_ask 用 sid + attach/peek clap role→sid)、commands.rs(resolve_chat_session_name/run_sessions 按 sid + SID 列)、mcp_serve.rs / mcp_session_tools.rs(forwarder 注入 _caller_sid)、mcp_chat_tools.rs(chat_history/send_input dead-chain TODO)。
- web:sessions_api.rs(history/collect by sid)、internal_hook.rs(X-Ccteam-Sid header)+ hook.sh。
- 测试:+8 验收/regression(event_pump_writes_turns、same-role no-bleed、sid-stable-across-restart、session_resolve vendor、verify_session_caller isolation、marker round-trip、permission session_sid ×2)+ 22 个 stale 断言更新(dedup INVERT、marker sid-keyed、env CCTEAM_CHAT_SID 等)。

## Remaining(留给后续阶段)
- **Phase 2** 直接建在 F1 上:B3-verify(BUG-3 按 sid 读已落,验证不串)· B4(session ls/status 从 gateway map 取活性+vendor+sid)· B5(pty_ws handle_session_ws 按 sid 解析 pane + send_keys/resize 走 default_backend)· F2(roleless:空 role 跳 --agent;spec_for_* 接口已为可空 role 预留)· F3(status 重写)。
- **收尾前**:real-claude /role smoke(#[ignore])+ W2 PermissionRequest 在 multiple same-role session 下的 hard-stop 复跑(hook 报对 sid、deny 只挡该工具)。
- **后续 cleanup(非 F1)**:删死 supervisor/outbound/BotSupervisor 链;chat_history 接 sid;per-sid IM 路由。
