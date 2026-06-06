# v0.8.7 Wave 4 — per-session web UI Handoff

> 直接 dev、无 PR、commit `v0.8.7:` 前缀。对应 PRD §4(Item D, DD.1–DD.3)/ dev-plan W4。
> **前置已满足**(用户 fix.md note):per-sid SSE tap(gap ①)早在 fix-round 落 dev,**未动**;本 wave 只补 gap ②(history)+ 前端整套 rewire。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1942/0**(W4 后端测试在 ccteam-web,不入此计数)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `cargo build -p ccteam-web`(vite SPA build via build.rs)OK · **vitest 108/108**(15 files,+29)· `cargo test -p ccteam-web` 仅 5 个已知 env-gated `ws_*` PTY 失败(sandbox 无 PTY)· `doctor --verify-mcp` 17/17、0 drift。

## 概要
每个 gateway session(`s{n}`)有独立页 + 历史 + 干净切换(不混流),全走 `/api/v1`(turn/SSE/stop)。`cto` 默认(FIX-2)保留;legacy operator 页(claude-N 命名)不动。

## Decided
- **后端只改 history(gap ②)**:`GET /api/v1/sessions/{sid}` 重写 —— 不再按 `session_id==sid` 过滤 progress.jsonl(永不匹配 `s{n}`);改 `guard.session_resolve(sid)`(W1,sync,克隆 role+project_dir,作 404 闸)→ **drop guard** → 读 `<project_dir>/.ccteam/chat/<role>/turns.jsonl`(`read_all_turns`)→ map `TurnRecord` 到 `{turn_id,ts,role,user,assistant}`(纯 helper `collect_session_turns`/`turn_to_event`)。未知 sid→404;缺文件/错→200 空。**lock discipline**(锁内解析、fs 读前 drop)。
- **前端整套 per-session rewire**:`ChatConsole.tsx` 按 `s{n}` keyed(`/chat/s/:sid` 路由 nest 进 App.tsx,`useParams`)。**弃**旧全局 web-chat WS + flat `ccteam.chat.rows.v1` buffer;改 **per-sid localStorage key** `ccteam.chat.rows.v2.${sid}`(`chatTranscript.ts` 纯模型),sid 变→ seed(`loadRows` + `getHistory`)+ SSE 重订阅(`setEvents([])`)→ **永不混流**。新 `sessionsApi.ts`(list `GET /projects/{slug}/sessions` · history `GET /sessions/{sid}` · turn/stop/create,same-origin fetch + Bearer interceptor)。新 `useSessionEvents(sid)`(泛化 useProgressStream → `/api/v1/sessions/{sid}/events`,cookie-auth EventSource,1→30s backoff)。切换器按项目 fan-out `listSessions`(`s{n}` 命名),**非** `/sessions/active`。
- **审批渲染 per-session**:W2 给 ChoicePrompt 事件打 `sid`+`options[]`;`useSessionEvents` 加 options,`eventToRow`→`kind:'approval'` 行,ChatConsole 渲染琥珀色「session sX 要跑…」+ 每选项一个按钮。
- **纯逻辑抽取**(chatTranscript/sessionsApi/useSessionEvents helpers)让 vitest 留 node-env(不需 jsdom),沿用 FIX-2 chatDefaults 模式。
- **add-only**:legacy SessionDetail/SessionsListPage/`sessions/active` 完全不 repoint;`cto` 默认保留;new-session create 限既有项目(create 端点 per-project),chat 路径删旧 `/newproject` WS flow。per-sid key 弃 flat v1 **无迁移**(pre-v1.0)。

## Rejected
- **DD.2 ③ 跨项目 `GET /api/v1/sessions` 聚合 SKIP**(非 trivial:route+view+client+tests)→ 用 SPA per-project fan-out(PRD 允许)。
- gateway-attached web 集成测试 history happy-path **不做**(FakeAdapter 是 ccteam-im `#[cfg(test)]`-only,ccteam-web 测试够不着;create_session_api 真 spawn adapter)→ 用纯 `collect_session_turns` 单测 + 既有 ccteam-im resolve+read pipeline 测试覆盖(同 sessions_api_test.rs 既定边界)。
- **不**自造 token-based REST 审批解析端点(超 W4 scope:后端只 history)→ 用既有 numeric short-reply 走 turn 端点 + 文档化 IM-path 限制。

## Risks
- ⚠ **web [Approve]/[Deny] 点击经 numeric short-reply 走 `POST /sessions/{sid}/turn`,但 `submit_to_sid` 直发 adapter、**不**经 gateway `resolve_numeric/resolve_selection`** → web 侧审批点击是 best-effort,**可能不能干净 resolve token 注册的 PermissionRequest pending**;robust 解析仍在 IM/WS token path(W2 已 live 证)。**渲染**per-session 正确完整;**resolve**是 follow-up(turn 端点→resolve_numeric + SSE payload 带 token)。
- EventSource 不能发 Bearer → per-sid SSE 用 `ccteam_token` cookie 认证(同 PTY WS);无该 cookie 则 SSE 401 而 REST 仍 work。
- ChatConsole + 2 个 data-load/SSE effect 触 eslint `react-hooks/set-state-in-effect`(3「error」,同既有 WorkflowView SSE 模式);**eslint 不在 CI**(check.yml 无 lint/npm step),exit 0,非阻塞 —— 留与 precedent 一致。
- 切换器 mount/refresh 每项目一次 `listSessions`(O(projects) 请求;聚合端点可收敛)。
- 5 个 `ws_*` pipe-pane PTY 测试需真 PTY(sandbox 不能流)→ CI/真机复测,非 W4 回归。

## Files
- ccteam-web 后端:`src/routes/sessions_api.rs`(history 重写 + 3 单测)、`tests/spa_assets_test.rs`(`/app/chat/s/{sid}` SPA fallback)。
- SPA:`web/src/App.tsx`(nest `/chat` + `/chat/s/:sid`)、`web/src/pages/ChatConsole.tsx`(整套 rewire)、`web/src/lib/sessionsApi.ts`(新)+`.test.ts`、`web/src/hooks/useSessionEvents.ts`(新)+`.test.ts`、`web/src/pages/chatTranscript.ts`(新)+`.test.ts`。

## Remaining
- **web 审批 resolve**(主 follow-up):turn 端点(或新 POST)经 gateway `resolve_numeric/resolve_selection` + SSE payload 带 token → web 点击可靠 resolve W2 pending(并入 W6 或 v0.8.8)。
- 可选 DD.2 ③:`GET /api/v1/sessions` 跨项目聚合 + `sessionsApi.listAllSessions` 替 fan-out。
- **W5**:OpenAPI 标注覆盖 session 端点(history/turn/stop/events/list) + role catalog/import(若加 web 端点)。
- **W6**:usage.md per-session web 用法;ws_* 真 PTY 机复测。
