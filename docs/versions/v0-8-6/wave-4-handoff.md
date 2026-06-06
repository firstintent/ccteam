# Wave 4 Handoff —— CLI 分组重构 + config + skill→~0

> v0.8.6 W4,stacked 在 v086-w2。两提交:**W4a** `2981e11`(clap 路由重组,behavior-neutral)+ **W4b**(config + skill 转化)。baseline:W4a 1881/0(零回归)→ W4b **1864/0**(净 -17:删 prefs/install-skill/persona 测试 + 加 5 config 测试)。clippy 0,fmt 干净。config 往返 + skills=0 已 smoke 确认。

## W4a —— CLI clap 重组(纯路由,handler 不变)
顶层:`init / start / stop / status / doctor` + `project` + `session` + `internal`(隐藏)〔+ `prefs`,W4b 折入 config〕。
- **project**:ls / show / new(原 flat)+ rm / stop(W3)。
- **session**:ls(原 `sessions`,live chat sessions)/ attach·pause·resume(原 flat)/ register·unregister·persona·add-tool(原 admin)/ **bots**(原 admin list-bots,registry 视图——避开 `session ls` 撞名)/ role。
- **internal(隐藏)**:mcp-serve / hook<sub> / hook-emit / peek / progress / send / spawn / attach / resume / mux / probe-project / web。
- **删 6 个废弃顶层别名** hook/peek/progress/send/spawn/mcp-serve;manifests(.mcp.json + 两 plugin)用 canonical `internal mcp-serve`;所有 CLI-invoking 测试改新路径。
- `session role <slug> <sid> <role>` = **薄指针**(bail 指向 IM `/role`)——真正的 CLI role 变更需 daemon-IPC(gateway socket),推后;IM `/role` 是真路径。

## W4b —— config + skill 转化
- **新 `config`(setup hub)**:bare `ccteam config` = TTY 交互式编号菜单;`config mcp`(装/刷 MCP,headless)、`config <key> <value>` / `config get <key>` / `config show`(非交互,preferences.toml 存储)。菜单项各自 dispatch 到与非交互**同一** action fn(可测,无需 TTY)。**吸收** `doctor --install-mcp`(MCP 装)+ IM token(`ccteam_im::onboarding::telegram_setup` → creds.json)+ prefs。
- **删** 顶层 `prefs` + doctor 的 `--install-mcp/--install-skill/--install-meta-agent/--install-all`;doctor 只剩诊断/自检/修复(含 --verify-mcp)。
- **itemE 收尾**:删最后 3 个 bundled skill(control/creator/im-setup)+ 整个 install 机制 + manifest skills 数组 → **0 个 bundled skill**(留 `skills/.gitkeep` 作项目自有 skill 扩展位)。`LEGACY_SKILL_NAMES`(8 名)保留,供升级时清旧 `~/.claude/skills/<old>`。
- **meta_agent_role.md** 命令串修正(`ccteam new→project new`、`remove→project rm`、`pause→session pause`、`ls→project ls`;ccteam-creator/control 引用改写为 project-new + cto 流)。

## Risks / Notes
- **marker_self_heal_test 既有并行 flake**(~1/10):进程级 marker_reporter registry 同 tuple 撞,单线程 100% 过;本 worktree 未碰该文件(CLAUDE.md §六 inotify/并行类)。非 W4 回归。
- `session role` CLI 是指针(非真变更),见上。
- **tier-1 docs 仍 stale**(usage.md 的 `prefs`/`doctor --install-mcp`、CLAUDE.md baseline)→ **W6 重写**。

## Files
- ccteam-cli:main.rs / commands.rs / mcp_serve.rs / 多 tests(+新 config_test.rs,删 install_skill_test.rs)。
- ccteam-core:skill.rs(缩成 LEGACY_SKILL_NAMES)/ lib.rs / meta_agent.rs / tool_surface.rs / templates/meta_agent_role.md(+删 persona_test.rs)。
- ccteam-im:lib.rs(`pub mod onboarding`)/ onboarding.rs。
- manifests:.claude-plugin/plugin.json + .codex-plugin/plugin.json(skills 清空,留 20 MCP tools);skills/ 删 3 目录留 .gitkeep。

## Remaining
- **W5(最大)**:标准资源 API(project/role/session + capabilities + SSE + 鉴权)+ per-session web;flex 类型 full EOL;API 落地后深砍 workflow 查看/控制类 MCP(→ ~10)。
- **W6**:tier-1 文档全量重写。
