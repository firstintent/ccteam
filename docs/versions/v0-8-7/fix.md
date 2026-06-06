# v0.8.7 fix.md — 实机使用发现的 bug/gap(dev session 随 v0.8.7 一起修)

> 来源:用户实机(IM + web)反馈,每条均 file:line 验证(**代码为 SoT**)。
> 格式:症状 / 根因(file:line)/ 修法 / 验收 / 归属。
> **基线 = `dev` HEAD `b4a6076`(W1 cto 调度 `0169ce1` + Lark/Feishu config `b4a6076` 已落)。所有 file:line 已对 b4a6076 校准。**
> 红线照旧:no prompt injection、不 scrape pane、永不主动 kill 长 session、`cargo fmt --all` + clippy 0 + baseline(W1 = 1877/0)不退。

---

## FIX-1 · 出站文件发送:正在聊天的 IM/web session 无法 `chat_send_file`(registry 与 live 绑定两套地址未对账)

**症状**:用户在 TG 跟 cto 聊天,让它发 `.md` 文件,`chat_send_file` 报 `no registered chat for <slug>/<role>`;cto 只能手动读 `gateway-state.json` + 调 `chat_register_bot` 绕过(自动得到 handle "Euclid")。"正在跟它聊的 session 还要手动注册才能发文件" = 缺失。

**根因(file:line 实证)= 两套地址存储,出站文件路径没打通**:
- **文字回复**走 session 的 **live 内存绑定** `GatewaySession.reply_to`(`ChatKey{channel,chat_id,user_id}`,inbound 时设)。turn 完私有 free-fn `pump_target(&GatewaySession)`(`gateway.rs:2034-2039`)读 `reply_to`→回退 `owner` → daemon `send_gateway_outbound`(`daemon.rs:773/880`)。**不碰 registry → registry 空也能回**(所以文字回复正常)。
- **`chat_send_file`(连 `interaction/ask`)**却只查**磁盘 registry**:`build_send_file_event` 里 `resolve_home_chat(slug,role,list_bots())`(`main.rs:2274`,error `main.rs:2275`;`interaction/ask` 同形 `main.rs:2388/2390`)读 `~/.ccteam/imd/registry/<slug>/<role>.json`。该 registry **只有显式 `chat_register_bot`(`mcp_chat_tools.rs:307`)/ `admin register-bot`(`commands.rs:4780`)才写**;inbound spawn 路径 `start_session`(`gateway.rs:956-1022`)**从不写它** → 空 → `None` → 报错。**W1 未改这条路**。
- ambient 寻址已注入:`chat_send_file` 的 stdio forwarder 把 `slug`/`role` 直接塞进 args(`mcp_serve.rs:486-490`),daemon 侧 `build_send_file_event` 已读到(`main.rs:2250-2257`)。

**✅ W1 已把前置接好**:`serve_mcp_socket`(`main.rs:2023-2032`)+ `handle_mcp_socket_connection`(`main.rs:2085-2091`)现在都带 `gateway: Option<GatewayHandle>` 形参(= 与 web AppState / DaemonArgs 同一 `Arc<Mutex<Gateway>>`,composition root `main.rs:1734-1749`,经 `mcp_gateway` `main.rs:1865-1867` 穿入)。dispatch 已把它给了 `execute_session_tool`(`main.rs:2131`)——但 **`execute_chat_send_file` 仍只收 `sink`、没收 gateway**(签名 `main.rs:2190-2193`;调用 `main.rs:2125` 没传)。所以前置满足,只差把 gateway 接到 file-send 这一支。

**修法(小而明确)**:
1. **穿 gateway 进 file-send 支**:`main.rs:2125` 改成 `execute_chat_send_file(&req, sink.as_ref(), gateway.as_ref())`;给 `execute_chat_send_file`(`main.rs:2190`)→`run_chat_send_file`(`main.rs:2213`)→`build_send_file_event`(`main.rs:2236`)加 `gateway: Option<&GatewayHandle>` 形参。`interaction/ask` 支同样穿。
2. **新增一个 pub gateway 访问器**(必须新增——`session_resolve` 只回 `{sid,role,project,project_dir}` 无 chat 目标;`pump_target`/`ChatKey`/`owner`/`reply_to` 全私有):例如 `Gateway::reply_target_for(project, role) -> Option<(String,String)>`,按 `(project==slug, role==role)` 找活 session(镜像 `start_session` 的 dedup find `gateway.rs:969-973`)→ 套 `pump_target` 的 `reply_to`→`owner` 解析返回 `(channel,chat_id)`。
3. **daemon 侧解析改序**:`build_send_file_event` 用现成 `slug`/`role` args 先 `gw.lock().await.reply_target_for(slug, role)`(**锁内只解析、drop guard 再做 fs/send**,照 `run_session_collect` `main.rs:2698-2701` 的 lock-discipline);`Some`→用 live 目标,`None`→回退 `resolve_home_chat`(registry)。
4. **registry 不删**(仍管 `@handle` 提及路由 + 非活跃时寻址命名 bot)。边界:同 `(project,role)` 多 chat 的消歧沿用 gateway 现有 dedup 模型(非本修新增)。

**验收**:TG/web 正在聊的 session 直接 `chat_send_file` 成功送回该 chat,**无需手动注册**;无活 session 时回退 registry;`interaction/ask` 同修;fake-adapter 测试覆盖「空 registry + 活 session → 送达」。

**归属**:随 **W1 之后的紧邻小修**(复用 W1 已穿好的 `gateway: Option<GatewayHandle>` + lock-discipline)。

---

## FIX-2 · web 默认 role=`assistant` 是**未定义 agent** → spawn 出无脑 pane、聊天不回复

**症状**:web 新建 session 默认 role=`assistant`,聊天无回复;`ccteam session ls` 每项目同时有 `…-assistant`(web 建)和 `…-cto`(IM/默认建),都 ALIVE。

**根因(HIGH 置信,file:line)**:
1. **web SPA 硬编码默认 `assistant`**:`web/src/pages/ChatConsole.tsx:699` `const effectiveRole = role.trim() || "assistant";`(role 初值 `""`);`ROLE_SUGGESTIONS` 也以 `assistant` 打头(`ChatConsole.tsx:71`)。提交时拼成 IM 命令 `/new claude assistant`。**Rust 后端无此默认**(`sessions_api.rs:163-169` 要求 role 非空、空则 400)。
2. **与产品默认 `cto` 不一致**:IM `/new` 无 role 时回退 `cto`(`gateway.rs:771`,但 web 总带显式 role 故绕过);`ccteam init` 只种 `cto.md`(`commands.rs:576` `DEFAULT_AGENT_SCAFFOLDS`);**全仓无 `assistant.md` 模板/常量/seed**。
3. **create 路径不校验 persona 是否存在**(W1 也未加):`spec_for_new` 无条件 `--agent <role>`(`claude_tui.rs:297-312`,故 `--agent assistant`);`start_thread` 只查 role 非空(`claude_tui.rs:422-426`);`start_session` 无 persona 校验(`gateway.rs:956-1022`,只在未知 **project** 时失败 `:988`);`run_session_spawn` 也只查 role 非空(`main.rs:2614-2619`)。**校验只在 `/role` 切换路径有**(`switch_current_role` `gateway.rs:1070-1074` 调 `ccteam_core::read_role`,不存在则拒绝)——从没应用到任何 create。
4. ⇒ web 建 `claude --agent assistant`(无 persona)→ tmux pane ALIVE(claude 进程在)但 **agent 未定义** → 不产出可转发的 turn → **无回复**。`…-cto` 来自 IM/默认创建(cto.md 存在,能回)。

**未实证(诚实标注)**:`claude --agent <未定义>` 的确切行为(报错退出 / 空 prompt 静默运行 / 挂起)是 **vendor 侧、不在本仓**。"ALIVE 但无回复"与"启动但不产 turn"一致,但精确失败态需 dev **用真 `claude` binary 实测** `claude --agent <未种的名字> --name x` 后再定校验信息措辞。

**修法**:
1. SPA 默认 `assistant` → **`cto`**(`ChatConsole.tsx:699`)+ `ROLE_SUGGESTIONS` 以 `cto` 打头(`:71`)。
2. 把 `/role` 已有的 persona 存在性校验(`ccteam_core::read_role`,def `roles.rs:138`,用法见 `gateway.rs:1070`)加到 **create 路径**(`start_session`/`create_session_api`,最好也覆盖 `run_session_spawn`):role 无 `.claude/agents/<role>.md` → **fail-fast 明确报错**("role 不存在,先 `/role` 或 `ccteam role add`"),而非 spawn 死 pane。防御纵深:任何 client(web/API/IM/cto-dispatch)送未种 role 都得明确错误。
3. 可选:`POST /api/v1/.../sessions` 后端兜底/默认 `cto`,与 IM 对齐。

**验收**:web 不选 role 默认建 `cto` 且能回复;建未种 persona 的 role → 明确报错、**不留死 pane / 不进 `session ls`**;`session ls` 不再冒出无脑 `assistant`。

**归属**:独立小修。**注:与 web 回复路由无关**——web-console 回复链经实证是通的(`reply_to`→`web`/`web-chat`、`WebChatChannel` 广播 `web_chat_bridge.rs:58-96`、`chat_ws.rs:128` recipient 过滤匹配;集成测试 `web_chat_bridge.rs:709-712` round-trip + 扛重启过)。

---

## FIX-3 · `ccteam status` 的 `STUCK` 是死的纯年龄启发式(**误报**,与 session 健康无关)

**症状**:`ccteam status` 两个项目都 `STUCK`(`age == last-event`,19m / 58m)。

**根因(file:line)**:
- `STUCK` = 纯空闲启发式:`silent_s >= 15min` → STUCK(`commands.rs:4099-4122` `stall_level`/`stall_verdict`;`stall.rs:33-35` 5/15/30 分层)。
- `silent_s = now - state.last_progress_event_at`,**字段为 `None` 时回退 `now - created_at`**(`queries.rs:241-244`)。
- **`last_progress_event_at` 全仓无生产写入** —— 仅测试里赋值过一次(`watchdog.rs:819`)。⇒ 实机永远 `None` → `silent = now - created_at` → **任何项目过了 15min 就 STUCK,与是否活跃无关**。
- chat turn 确实写 `progress.jsonl`(`chat_progress.rs:111-120/201`),但 `append_event`(`progress_bridge.rs:42-55`)从不动 `state.json` → stall 时钟永不前进。

**⇒ 重要校准**:用户看到的"都 STUCK"是**误报**,**不证明 assistant session 坏了**——STUCK 是项目级、纯年龄;能正常工作的 cto session 所在项目同样会 STUCK。真正的功能 bug 是 FIX-2。

**修法**(二选一):(a) progress 事件 `append_event` 时同步 bump `state.last_progress_event_at` + save;或 (b) stall 时钟改从 `progress.jsonl` 末行 ts / mtime 取,弃用从不写的 state 字段。

**验收**:刚聊过/活跃的项目不显示 STUCK;STUCK 只在 event 真停止推进 ≥ 阈值时出现。

**归属**:独立小修。

---

## Notes for dev(非 bug,顺带校正,免得重做已完成的活)

- **W4 dev-plan 校正**:dev-plan W4 后端「3-gap ① gw_event SSE tap」**其实已完成**——SSE `GET /api/v1/.../sessions/{sid}/events` 已正确按 sid 过滤 gateway broadcast(`sessions_api.rs:304` 处理器、`:367-369` `event_matches_sid` `ev.sid==Some(target)`;gateway 给事件打 `sid:Some(s{n})` `gateway.rs:1196`/`:1481`)。这正是 review #2「SSE broken」、**fix-round 已修对**。W4 真正剩的:② `GET /sessions/{sid}` 历史读 `.ccteam/chat/<bot>/turns.jsonl`(现 filter `session_id==s{n}` 永不匹配、返回空:`sessions_api.rs:237` + s{n} 是内存计数器不写 progress.jsonl,mint `gateway.rs:983`)+ 前端整套 per-session rewire。W4「前置:等 fix-round SSE」**已满足**,不必再等。
