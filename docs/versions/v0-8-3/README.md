# v0.8.3 — web 端进入层(WS)

v0.8.3 把 web 从只读看板推进为与 IM 平行的用户进入层:浏览器通过
`ccteam-chat.v1` WebSocket 接入同一个 Gateway 路由核,复用
`chat ⇄ project ⇄ session`、`/new`、`/use`、`/cd`、`@handle`、
`/compact`、`/review` 与 resume-by-id。

## 落地内容

- `ccteam-web` 新增 `GET /ws/chat` 与 `ccteam-chat.v1` 帧类型。
- `ccteam-cli` 在 `run_start` 装配 CLI-owned mpsc/broadcast bridge,
  将 web-local JSON 转成 `ccteam-im::transport::ChannelMessage`;
  `ccteam-web` 与 `ccteam-im` 仍互不依赖。
- web chat outbound 增加 per-recipient backlog,daemon 重放
  `~/.ccteam/imd/outbound.jsonl` 时即使浏览器暂时离线也可在重连后补发。
- SPA 新增 `/app/chat`:项目/session 侧栏、Chat/Terminal 切换、transcript、
  composer 与 `/new` 快捷入口。
- `web` channel 进入 IM ACL local websocket allowlist,与 dev-plan 要求的
  `channel="web"` 对齐。

## 验收

- `cargo test -p ccteam-web chat_frame`
- `cargo test -p ccteam-web --test chat_ws_test`
- `cargo test -p ccteam-cli web_chat_bridge`
- `cargo test -p ccteam-cli web_chat_ws_routes_through_gateway_and_survives_restart`
- `npm run build` in `crates/ccteam-web/web`

完整 P6 gate 以 ship turn 的命令输出为准。
