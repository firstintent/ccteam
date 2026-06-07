# v0.8.8 — 独立 session 模型 + web 能力补全 + 实机 bug 批修(冻结归档)

> 冻结里程碑。当前架构 SoT = 根 `CLAUDE.md` + `docs/tech-design.md`(协议以代码为准)。本目录另含:`prd.md`(需求)· `bug.md`(实机 bug 根因)· `dev-prompt.md`(执行 briefing)· `wave-0..4-handoff.md`(每阶段 Decided/Rejected/Risks/Files/Remaining)。

## 一句话
把 **session 从「= role」升成独立一等实体 + 持久 sid**(根治 per-session 历史串台 BUG-3)+ 补全 web(config / role 页 / status 重写 / 终端修复)+ 批修 v0.8.7 实机 bug + 代码库清理。

## headline:F1 — session 是一等实体(keystone 改动)
- session = 独立一等实体,持久 `sid`(`s<N>`,单调、扛 daemon 重启、不复用);**role 降为属性**。
- 去 `(project,role)` dedup → 同 role 可并存多个独立 session(各聊各的)。
- pane(`ccteam-chat-<slug>-<sid>`)/ turns(`.ccteam/chat/<sid>/turns.jsonl`)/ marker 全按 sid;`CCTEAM_CHAT_SID` 注入 pane env(daemon HTTP 路径加 `X-Ccteam-Sid` header)→ hook/forwarder 报 sid、写读同键。resume-by-session-id。
- **关键修正(recon 发现、PRD 漏)**:live daemon 此前**根本不写 turns.jsonl**(唯一 writer BotSupervisor 是死代码、未 wire)→ F1 在 gateway `spawn_event_pump` 补按 sid 的 turns writer,否则只修读侧会让历史永远空。

## 交付清单
- **F2 roleless**:空 role → spawn 不加 `--agent`(裸 claude,brain 走项目 CLAUDE.md);后端通,web "无角色" 选项落地。
- **F3 status 重写**:列所有项目 + 各自 sessions(role/vendor/status/sid/last-event)、删 recent events、两行 web token/url(LAN ip)。
- **F4 web config**:`/api/v1/config/im`(web-token 门后,telegram+lark,masked 不回显明文,chat_id 异步,restart_required 非热生效)+ Settings 页。
- **F5 web role 浏览页**:只读(Roles tab,列表 + 详情)。
- **B1**:`project stop` 走 ProcessBackend(默认 rmux 真停)· **B2**:web 新建项目入口(REST)· **B3**:新建弹窗 role 真实下拉 · **B4**:`session ls`/`status` vendor+sid 从 gateway map(codex 不再误报)· **B5**:web 终端按 sid 解析 pane + `default_backend` · **BUG-3**:F1 根治。
- **C1 清理**:删 teams/skills/examples/根config.yaml/workflows-qa-autoloop/agents-explorer/tests-intent-corpus/host-probe脚本(保留 .agents/ + agents/__lead.md + workflows/dev-flow 因 build/test 硬依赖)。
- **实机 bug 二修(用户测 dev 发现)**:① 新建项目后建 session 报 `unknown project` —— `start_session`/`switch_current_role` 查项目前先 `ensure_project_loaded()`(从 config.yaml SoT 同步,跟 `/cd` 一致);② init 模板 `settings.local.json` 的 `permissions.allow:["*"]`(新版 Claude Code 拒)→ 改合法 `permissions.defaultMode:"bypassPermissions"`。

## 收尾 gate
- `cargo test --workspace --exclude ccteam-web` **1998/0**(v0.8.7 基线 1975 → v0.8.8 1998)· clippy --workspace --all-targets **0** · `cargo fmt --all` 干净 · ccteam-web **224 pass + 4 env-gated `ws_*`**(tmux pipe-pane,留 CI/专机)· SPA vitest **151/151** · `doctor --verify-mcp` **17/0**(F4 是 REST 路由,不增 MCP 工具)· skill-gate 0 命中(skills/ 已删)· 版本 0.8.7→0.8.8(workspace + 4 个 plugin manifest 站点)。

## ship-gate-pending(专机 / 真 claude 验,非沙箱可跑)
- **/role real-claude smoke**:F1 后 `/role` 同 sid → `--name` 不变,旧 role 的 claude jsonl 仍在该 name 下。已实现 carry-context + fresh-spawn 分支 death-probe;真机 `--name`-collision 行为(carry vs error)需真 claude 验(沙箱 fake 复现不了)。
- **W2 PermissionRequest 复跑**:multiple same-role session 下 hitl hook 报对 sid、deny 只挡该工具不杀 turn。
- 4 个 `ws_*` PTY + 真 per-session 字节中继:需 tmux pipe-pane / live gateway,留 CI/专机。

## 迁移(pre-v1.0 无兼容)
session 模型不兼容旧数据 → 升级前 **清 `~/.ccteam` + 各项目 `.ccteam` → 重 `ccteam init`**(旧 per-role 历史丢弃,可接受)。bug2 的 `allow:["*"]` 修复只对新 init / 重 init 生效;已有项目需手删该行或重 init。

## 后续(deferred,非阻断,可 v0.8.9)
dead-chain cleanup(删 supervisor/outbound/BotSupervisor + chat_history/send_input 死工具,F1 已 TODO 标)· per-sid IM 路由 · catalog 在线浏览/装(web)· ChatConsole 裸色统一 token。
