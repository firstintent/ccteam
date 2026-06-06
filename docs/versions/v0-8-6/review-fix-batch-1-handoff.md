# v0.8.6 Review-Fix Batch 1 (🔴 HIGH) Handoff

> worktree `v086-fix`(off origin/dev=cca83ca)。一提交 `v0.8.6 fix:`。baseline 1861 → **1867/0**(+6 测试)。clippy 0 含 web,fmt 干净。ccteam-web 仅 5 个 env-gated ws_* fail(pipe-pane,非本批)。

## #1 [安全] roles API 路径穿越 —— 修复 + 端到端验证
- 根因:`GET /api/v1/projects/{slug}/roles/{role}` 读路径对 role 零校验;axum 单次 percent-decode → `..%2f..%2f.claude%2fCLAUDE.md` 变字面 `../../.claude/CLAUDE.md`,`read_role` 的 `agents_dir.join(...)` 解析穿越 → 读任意 *.md。写/PUT 路径本已用 `validate_bot_name`(charset `[a-z0-9_-]`)。
- 修:在 **core `read_role` 顶部**(`validate_role_name`)+ **web `handle_get_role` 顶部**(`role_name_is_valid`,返回 **400** 非 404/500)双层校验,charset 与写路径一致(拒 `/` `\` `..` 前导`.` 空)。
- 测:core `read_role_rejects_path_traversal` + web inline guard test + **HTTP `resource_api_test.rs::get_single_role_rejects_path_traversal`**(植入 out-of-tree canary + /etc/passwd + ~/.claude/CLAUDE.md,断言 400/404 无泄漏,正常 cto 仍 200)。**live HTTP smoke 确认**:穿越 → 400 无泄漏。

## #2 per-session SSE 零事件 —— 改走 gateway 事件流
- 根因:`/sessions/{sid}/events` 订 file-watcher EventBus(progress 写 flat `<slug>.jsonl`、sid=None;`is_session_sid` 不认 `s{n}`)→ 真 session 只发 keep-alive。
- 修:gateway 加 `GatewayEventSink` tee(mpsc + broadcast)+ pub `subscribe_events()`;每个 GatewayEvent 既走原 IM mpsc 路径(**不破坏**)又入 broadcast(cap 256)。`set_event_sink` 公共签名不变。sessions_api `/events` 改:有 gateway → 订 `subscribe_events` 按 `event.sid == Some(sid)` 过滤 → `event: progress` 帧 + 15s KeepAlive;无 gateway 仍 `gateway_unavailable`。
- 测:gateway tee/subscribe 单测 ×3 + sessions_api sid-filter/mapping 单测 ×2(live 端到端 SSE 由 batch-2 #8 smoke / 手动覆盖)。

## #3 init 建死目录 → doctor 立报 drift —— D1.1 落地
- 修:`run_init` + `bootstrap_project_at_dir` 改建 **恰好 `canonical_home_dirs()`**(hooks/progress/run/state),删 `write_global_helper_templates`(`HELPER_TEMPLATES` 空)+ main.rs 每次 start 的伪 `phases dir not found` 警告。
- 测:`run_init_leaves_no_home_layout_drift`(fresh init 后 drift 空)+ `bootstrap_project_does_not_create_templates_dir`。**live 确认**:fresh init → doctor drift 段空、legacy-skill 段无误报。
- 确认:`~/.ccteam/{phases,templates,inbox,control}` 的活 reader(run_remove cleanup 等)对缺失目录 no-op;deferred orchestrator 不跑。

## #4 旧 skill 清理够不着 + #7 LEGACY 漏项
- 修:`migrate_legacy_skill_dirs` 挂到 **`run_doctor()` 常驻尾**(`render_legacy_skill_cleanup_line`)→ 每次 `ccteam doctor` 清升级残留 `~/.claude/skills/ccteam-*`。#7:`LEGACY_SKILL_NAMES += ccteam-team, ccteam-scan`。
- 测:`migrate_legacy_skill_dirs_removes_team_and_scan`。

## Files
- ccteam-core:roles.rs(#1)、skill.rs(#7)、tool_surface.rs(#4)、projects.rs(#3 helper-templates 删)。
- ccteam-web:routes/roles.rs(#1)、routes/sessions_api.rs(#2)、tests/resource_api_test.rs(#1 HTTP)。
- ccteam-im:gateway.rs(#2 tee + subscribe_events)。
- ccteam-cli:commands.rs(#3 init + #4 doctor)、main.rs(#3 警告删)。

## Risks / Notes
- read_role 现对非法 role 返回 Err(原 Ok)——唯一 in-repo caller 是 web(已转 400);若有其他 caller 需处理 Err。
- validate 规则在 core/web 各复制一份(镜像私有 `validate_bot_name`);未来可把 `validate_bot_name` 提 pub(crate) 单一源。
- #2 live 端到端 SSE 需真 gateway+claude(batch-2 #8 #[ignore] smoke / 手动)。
