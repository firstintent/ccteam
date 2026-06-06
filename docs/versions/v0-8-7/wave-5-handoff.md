# v0.8.7 Wave 5 — OpenAPI 自动文档(utoipa-axum + Scalar)Handoff

> 直接 dev、无 PR、commit `v0.8.7:` 前缀。对应 PRD §5(Item E, DE.1–DE.4)/ dev-plan W5。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1942/0**(W5 测试在 ccteam-web,不入此计数)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `cargo build -p ccteam-web` OK · `cargo test -p ccteam-web` 仅 5 个 env-gated `ws_*` PTY 失败,**新 openapi_test 3/3 过** · vitest 108/108 · `doctor --verify-mcp` 17/17、0 drift。

## 概要
一个 URL `GET /api/docs`(Scalar 交互式)看 `/api/v1` **全量 28 ops**;spec `GET /api/v1/openapi.json` 由**同一套路由注册**生成(单一来源、防漂移)。

## Decided
- **单一聚合 `OpenApiRouter`**(`routes/openapi.rs`,`split_for_parts()`)= 既出 live `Router` 又出 spec,**同一注册集**(真单源/anti-drift);`stateful_router` 里 7 处 per-module `.merge(router())` 换成一处 `.merge(openapi::api_v1_router())`。
- **deps**:`utoipa 5.5.0`(axum_extras,chrono)+ `utoipa-axum 0.2.0` + `utoipa-scalar 0.3.0`(axum)—— pin 在 workspace `[dependencies]`、ccteam-web `.workspace=true`(合仓约定);**对 axum 0.8 编译干净**(避开 pin axum 0.7 的 utoipa-axum 0.1.x 线)。
- **全 `/api/v1` 注解**(28 ops):capabilities / projects(GET/POST,{slug} GET/DELETE)/ roles(GET,{role} GET/PUT)/ sessions(GET/POST,{sid} GET,turn POST,events SSE,stop POST)/ auth token / artifact_queue+status / cost_history / jobs/{id}/log / sessions/active / teams(5)+ teams events SSE。每 handler `#[utoipa::path]` + `pub(crate)`、删各自 `router()`;req/resp 结构体 `#[derive(ToSchema)]`、query `IntoParams`。
- **serve**:`GET /api/v1/openapi.json`(OpenAPI 3.1 文档,`Extension(Arc<OpenApi>)` build 时注入)+ `GET /api/docs`(Scalar UI,`Scalar::with_url`)。
- **DE.3 鉴权**:两者 mount 在 `stateful_router` 内,**同一 `auth::auth_layer` web-token 门**裹住(无公开未鉴权 spec)。测试证:无 `Authorization: Bearer ccteam:<hex>` → 401,有 → 200。
- **SSE 表达**:两个 SSE GET(sessions/{sid}/events、teams/{name}/events)声明 200 `content_type="text/event-stream"`(OpenAPI 无法建模 SSE body)+ description 记 frame 形状(`event: progress`,data = `{id,sid,kind,content,done?,options?}`)+ no-gateway 发一帧 `gateway_unavailable` 仍 200。

## Rejected
- 不留 per-module `router()` 与 OpenApiRouter 双轨(会漂移)→ 单源聚合。
- 不开公开未鉴权 spec(DE.3 决策:与 /api/v1 一致鉴权)。
- 不用 pin axum 0.7 的旧 utoipa-axum 线。

## Risks
- 行为保持(同路由/鉴权/响应);唯一结构变化 = 路由注册从 7 个 merge 收敛成 1 个聚合 router —— drift 测试 + serve/auth 测试守。
- `routes_annotated` 列表 28 项;drift 测试 frozen expected list 钉 op 数(narration 提到 27/28 小出入,以**测试断言为准、已绿**)—— 任何加/删路由不注解会改 op 数、测试红(强制 dual-edit)。
- Scalar UI 在 web-token 门后(HTML 页需 cookie/token);与全站一致。

## Files
- `Cargo.toml`(workspace 3 dep)+ `Cargo.lock`。ccteam-web:`Cargo.toml`(3 `.workspace` dep)、`src/routes/openapi.rs`(新,聚合 + serve)、`src/routes/mod.rs`(声明 + 收敛 merge)、`api_v1.rs`/`projects.rs`/`roles.rs`/`sessions_api.rs`/`capabilities.rs`/`teams_api.rs`/`teams_sse.rs`(注解 + pub(crate) + 删 router())、`src/views.rs`(4 view ToSchema)、`tests/openapi_test.rs`(新,3 测试)。

## Remaining
- **W6**:usage.md 写 `/api/docs` 一站式 API 文档;tech-design「协议→代码」加 openapi 指针。
- 若将来给集成方公开 spec(DE.3 备选),可单独放开 openapi.json 鉴权(当前有意一致鉴权)。
- W3 role catalog/import 的 web 端点(deferred to CLI)若后续加,记得带 `#[utoipa::path]`(drift 测试会强制)。
