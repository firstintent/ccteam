# v0.8.6 Review-Fix Batch 2 (🟠 MEDIUM) Handoff

> worktree `v086-fix`(承 batch 1)。一提交 `v0.8.6 fix:`。baseline 1867 → **1870/0**(+3,16 ignored)。clippy 0 含 web,fmt 干净。ccteam-web 234 pass / 5 env-gated ws_*。

## #5 /role 打错/缺 role 会毁活会话 —— validate-before-teardown
- 根因:`switch_current_role` 先拆旧 pane(event_pumps.remove + close_thread)再 respawn,对 role **零校验** → 打错/缺 role 时 `claude --agent <bad>` 失败但会话已拆 → 用户静默丢工作会话。
- 修:在**任何拆解之前**(解析 cwd 后)插入校验门 —— 用 `ccteam_core::read_role(&cwd, &role)`(同时校验 name charset `[a-z0-9_-]` + `.claude/agents/<role>.md` 存在);非法/缺失 → 返回中文 Err「role 不存在:.claude/agents/<role>.md 未找到;用 /role <已存在的角色>」,**当前会话完全不动**。no-active-session / same-role no-op 路径保持在前。
- 测:`gateway_role_missing_role_keeps_session_intact`(/role ghost → Err + 会话不变,fake.starts 仍 1、/sessions 仍 s1:cto、follow-up 仍 echo;再 /role reviewer 正常)+ 更新 `gateway_role_switches_current_session_in_place`(用 tempdir + seeded reviewer.md)。

## #6 init 覆盖用户 settings.local.json —— read+merge
- 根因:`write_settings_template` 渲染完整 base config 后**裸 `fs::write`** → re-init/就地 init 抹掉用户私有(gitignored)`.claude/settings.local.json`(自带 hooks/env/permissions)。
- 修:改 read+merge(照 `ensure_chat_hooks_installed`):读现有(缺/坏 → `{}`)→ `merge_ccteam_settings()` 合 ccteam 托管键 → 写回。深合并键 `CCTEAM_DEEP_MERGE_KEYS = [env, permissions, enabledPlugins]`(保留嵌套用户键);`hooks` 整块由 ccteam 拥有(与 ensure_chat_hooks 一致)。public 签名不变。
- 测:`merge_ccteam_settings_preserves_user_keys` + `write_project_settings_merges_into_existing_local_file`(端到端:env.USER_X / permissions.ask / model 存活,ccteam 键叠加)。
- **Caveat**:`permissions.allow` / `permissions.deny` 是模板渲染键 → 用户自定义的 allow/deny 在 re-init 时仍被 ccteam 覆盖(`permissions.ask` 及其它嵌套保留)。ccteam 托管 agent 权限策略,可接受;若要保留用户 deny 需后续细化。

## #8 keystone 无 smoke —— 加 #[ignore] 真 claude smoke
- 新文件 `crates/ccteam-harness/tests/claude_agent_smoke_test.rs`,测 `claude_agent_session_role_keystone_smoke`,`#[ignore]`-gated(默认 gate **不跑**,不影响计数;`-- --ignored` 手动跑)。
- 模型:照 `tmux_backend_session_roundtrip`(tmux gate)+ `f10_real_codex_stdio_new_smoke`(#[ignore] 真 vendor + serial)。
- 做:真 claude 下 seed temp 项目 `.claude/agents/<role>.md`(带识别 token)+ `settings.local.json`(hook 记事件)→ ClaudeTuiAdapter start_thread(`claude --agent <role> --name`)→ submit turn → 断言 (a) transcript 含 persona token(--agent 加载 role)(b) hook log 出现 Stop(turn 完成);再 resume(kill + `--resume`)+ follow-up。**transcript+hook 取证,不 scrape pane**(红线合规)。guard:claude/tmux 缺则早返回(hermetic CI 绿)。

## Files
- ccteam-im:gateway.rs(#5)。
- ccteam-core:templates/mod.rs(#6)。
- ccteam-harness:tests/claude_agent_smoke_test.rs(#8,新)。

## Risks / Notes
- #6 permissions.allow/deny caveat(见上)。
- #8 真端到端需登录的 claude(本沙箱不跑;CI/专机 `-- --ignored`)——也覆盖了 #2 的 live SSE 路径(手动)。
