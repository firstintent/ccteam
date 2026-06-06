# v0.8.7 fix.md — 实机使用发现的 bug/gap(dev session 随 v0.8.7 一起修)

> 来源:用户实机(IM + web)反馈,每条均 file:line 验证(**代码为 SoT**)。
> 格式:症状 / 根因(file:line)/ 修法 / 验收 / 归属。
> **基线 = `dev` HEAD `b4a6076`(W1 cto 调度 `0169ce1` + Lark/Feishu config `b4a6076` 已落)。所有 file:line 已对 b4a6076 校准。**
> 红线照旧:no prompt injection、不 scrape pane、永不主动 kill 长 session、`cargo fmt --all` + clippy 0 + baseline(W1 = 1877/0)不退。

---

## FIX-1 · 出站文件发送:正在聊天的 IM/web session 无法 `chat_send_file`(registry 与 live 绑定两套地址未对账)

> ✅ FIXED in 935eb66 —— 新 `Gateway::reply_target_for(project,role)` + `gateway` 穿 file-send/ask 支,live session 优先、registry 兜底(`resolve_live_reply_target` lock-discipline)。

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

> ✅ FIXED in 935eb66 —— SPA 默认 role `assistant`→`cto`(`chatDefaults.ts`)+ create chokepoint `Gateway::start_session` 加 `ensure_role_exists`(未种 role fail-fast、不留死 pane)。

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

> ✅ FIXED in 935eb66 —— `summary_from_state` stall 时钟改从 `progress.jsonl` 末行 ts 取(`last_progress_event_ts`)并回填 state 字段;刚活跃项目不再误报 STUCK。

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

---

## 审查发现 — v0.8.7 W1–W6 实现复审(post-ship, pre-tag;2026-06-06)

> 5 路并行 reviewer + 对抗验证 + 我独立从冷 target 复跑 gate。**Gate 绿(独立复现)**:`cargo test --workspace --exclude ccteam-web` = **1942/0** · clippy **0** `-D warnings` · `cargo fmt --all` 干净。**无 blocker。** 下列按严重度,每条 file:line + 修法;severity 是我对抗校准后的判定(reviewer 原判注括号)。

### R-H1 [HIGH] web HITL「批准」按钮是**骗人的**:点了显示"已回应",实际 600s 后超时→deny
- **file:line**:`web/src/pages/ChatConsole.tsx:181-198`(`resolveApproval`→`submitTurn(sid, String(idx+1))`)+ `crates/ccteam-web/src/routes/sessions_api.rs:326-356`(`handle_session_turn`→`submit_to_sid`,**不过** `resolve_numeric`/`resolve_selection`)。
- **问题**:W2 审批气泡在 web 正确渲染,但点[批准]是把字面 "1" 当**新 turn** POST 进去,永不 resolve token-keyed pending → 阻塞的 `PermissionRequest` hook 等满 600s → **降级 deny**;UI 却立刻翻"已回应"、"1" 还作杂键注入 pane。**任何能在 web 看到的 hitl session 都中招**(含 IM `/new … hitl` 建、web 端看)。dev 自承的 caveat,但 blast radius 更大 = **误导性**(以为批了其实拒了 + 卡 10 分钟)。两 reviewer 独立 trace 一致 + dev 自承。
- **修法**:① 加专用 resolve 端点 `POST /sessions/{sid}/resolve {token,idx}`(或 token 经 SSE 带前端再回),走 gateway `resolve_selection`/`take_by_token`;**未修前**:web 端**隐藏/禁用** hitl 审批按钮,改提示"去 IM 批准"。
- **置信**:high,全链 code 验证。**建议这是 tag/merge 前唯一必先处理的(至少诚实化 UI)。**

### R-M1 [MEDIUM] cto 权限门可伪造(`_caller_role` 取自 args + MCP 绕 allow-list)——handoff"不可伪造"说法不成立
> reviewer 判 HIGH;我**对抗校准后下调 MEDIUM**,理由见下。
- **file:line**:`crates/ccteam-cli/src/main.rs:2939-2960`(门读 `args["_caller_role"]`);socket 无 peer 校验 `daemon.rs:62` + `main.rs:2134-2141`;anti-spoof 只在 `mcp_session_tools.rs:180-185` stdio forwarder 成立。
- **问题**:硬门信任请求里的 `_caller_role`;daemon 无法验证 socket 对端就是 forwarder。任意同用户进程(work-role 经 Bash)可 `nc -U ~/.ccteam/run/mcp.sock` 发 `{"_caller_role":"cto"}` 过门拿全 session 操控。
- **为何只 MEDIUM(诚实范围)**:当前所有 agent 同 OS 用户 + Skip 模式带 `--dangerously-skip-permissions` + Bash —— 伪造 cto 拿到的"驱动他 session"能力**不超过它已有的 Bash**(非越权获得新能力),**今天不可利用**。但 wave-1-handoff 宣称"agent **不能**伪造身份、真正边界是 daemon role 门(稳)"——**该说法错**(且 MCP 工具本就绕 Claude per-agent allow-list,layer-1 对 MCP 也无效)。**HITL / 未来 sandbox 化 work-role(Bash 被 gate)时这变真漏洞**。
- **修法**:别从 socket-payload 取权限——spawn 给 pane 注入 per-session secret token(随 `CCTEAM_CHAT_ROLE`),daemon 存 `sid→{role,secret}`,门校验 token 而非明文 role。**最低**:现在就把 handoff/红线"不可伪造"措辞改诚实(单用户全信任下非边界)。
- **置信**:high(门逻辑/无 peer 校验/agent 有 Bash 全 in-tree 证实);exploit 一行未实跑。

### R-M2 [MEDIUM] `/new … hitl` 命中已存在 (project,role) pane:回执报"hitl"但 pane 仍跑 skip-permissions
- **file:line**:`gateway.rs:799-804`(回执按**请求** mode 拼)vs `gateway.rs:1010-1022`(dedup 复用旧 session、**忽略**请求 mode)。
- **问题**:首条消息 auto-spawn skip cto;之后 `/new claude cto hitl` 命中同 (project,cto),回执"created s1 (hitl)"——但 s1 还是 skip pane、无 hook。**用户以为全工具受监督,实际一个没有**(危险方向)。
- **修法**:`start_session` 复用时返回**实际** mode;`/new` 按实际拼(命中 skip 时回"reusing s1(仍 skip——停掉重建才能 hitl)")。置信 high。

### R-M3 [MEDIUM] cto 门只按 role、从不限 project → 跨项目操控 session
- **file:line**:`main.rs:3017-3028`(`session_spawn` 收显式 `project` 不校验与 `_caller_slug` 一致);dispatch/collect/stop 收任意 `sid`(`:3060/3088-3097/3187-3189`);`gateway.rs:1023-1027` 从**全局** projects map 解析。
- **问题**:绑项目 A 的 cto 可 `project:"B"` spawn、可对**任意** `s{n}`(含别 user-chat 建的)dispatch/collect/stop。
- **修法**:`session_spawn` 拒 `project != _caller_slug`(或删 project 参数);dispatch/collect/stop 先校验 `session_resolve(sid).project == _caller_slug`。(依赖 R-M1 可信身份才有意义。)置信 high。

### R-M4 [MEDIUM] `project rm --purge` 漏清 `PermissionRequest` hook(purge ≠ init 逆,违红线)
- **file:line**:`crates/ccteam-core/src/tool_surface.rs:880-890`(`hook_command_is_chat_hook` 只匹配 `chat-progress`/`intercept-ask`,漏 `permission-request`)。
- **修法**:谓词加 `permission-request` + 回归测试断言 purge 清掉 PermissionRequest 段。置信 high。

### R-M5 [MEDIUM] Scalar 文档 UI 从 unpinned CDN(`cdn.jsdelivr.net`)取 JS——离线打不开 + 供应链 + CSP
- **file:line**:`crates/ccteam-web/src/routes/openapi.rs:123`(`Scalar::with_url` 用默认 `DEFAULT_HTML`,内含 `<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference">`)。
- **问题**:ccteam 主打"自己机器常驻"(常锁网 NAS/LAN)→ `/api/docs` 离线/防火墙后**空白**;第三方 CDN 未 pin/无 SRI。(`openapi.json` 不受影响、auth 已正确门控。)
- **修法**:自托管 Scalar bundle(`.custom_html(include_str!)` + vendored JS),或至少 pin 版本 + `integrity=`。置信 high。

### R-M6 [MEDIUM→LOW] FIX-2 web API 对未种 role 返回 500(应 4xx)
- **file:line**:`crates/ccteam-web/src/routes/sessions_api.rs:217-221`(`create_session_api` 全错误→500)。SPA 默认 cto 故 happy-path 不中;仅 modal 手敲坏 role 触发。
- **修法**:边缘区分 persona-missing(→400/422)与真内部错(→500)。置信 high。

### R-M7 [MEDIUM→LOW] FIX-3 每次 status/ls/dashboard 全量读 progress.jsonl(O(file)、无 rotation)
- **file:line**:`crates/ccteam-core/src/progress.rs:52`(`last_event` `read_to_string` 整文件)经 `queries.rs:270-277` 每项目调一次,落 `collect_projects()`(每 `ccteam status`/`ls`/web `GET /api/v1/projects`)。progress.jsonl 全仓**无 rotation/cap**。
- **修法**:tail-read 末行(seek EOF 反向有界块)而非整文件 slurp;或给 progress.jsonl 加 rotation。置信 high 是 O(file);medium 是否实际成问题。

### 低/nit(→ v0.8.8 顺手)
- **R-L1** [low] hitl 无 registered chat 或用户不答 → 每非 allowlist 工具卡 ~600s 才 deny(fail-closed 但无 operator 信号)。修:permission prompt 更短 TTL + outstanding 发 progress 行。`main.rs:2545-2549`。
- **R-L2** [low] FIX-3 对**从没聊过**(init 但无 event)项目仍 STUCK(保留 `unwrap_or(age)`,`queries.rs:255-257`)。修:init 种 baseline event 或"never-started"单独判定。
- **R-L3** [low] `session_collect` >20 turn 爆发时 cursor 跳过中间 turn(`main.rs:3137-3143`)。修:有丢弃则 cursor 设边界或回 `truncated:true`。
- **R-L4** [low] role import `resp.text().await` 无大小上限(OOM;受信 host+30s timeout 缓解)。`role_import.rs:162`。修:streaming + ~1MiB cap。
- **R-L5** [low] role import 跟随重定向到任意 host(reqwest 默认 follow 10)。`role_import.rs:140-149`。修:`.redirect(Policy::none())` 或限同 host。
- **R-L6** [low] role import 无内容信任提示 + `AGENCY_RAW_BASE` 跟 `HEAD` 未 pin sha(`role_catalog.rs:53`)。修:pin sha + 装完提示"用前 review 该 .md"。
- **nit**:token 40-bit time-only 同窗碰撞(`main.rs:2607-2618`,pre-existing D6);role 默认 stem 跨 division 碰撞(`--force` 会 clobber 同名,已 gated+报告)。

### needs-runtime(非 fix,验证缺口)
- **PermissionRequest `behavior:allow|deny` 契约未对真 binary 验证**:`#[ignore]` smoke 只证 hook **fires**,没证真 claude 认 `{behavior:"deny"}` 真挡 / `"allow"` 真放。dev 跑 `cargo test -p ccteam-harness --test claude_agent_smoke_test -- --ignored` 并断言 deny 时受害文件未删 / allow 时删。`claude_agent_smoke_test.rs:458-528`。

### 复审确认"干净"的(给信心,不用动)
- **FIX-1/2/3 三核心实现全部正确+完整**:FIX-1 lock-across-await 干净(`main.rs:2348-2366` resolve 锁内、drop guard 再 fs/send)、registry fallback 在、interaction/ask 同修;FIX-2 `ensure_role_exists` 在唯一 chokepoint `start_session`(`gateway.rs:2144`,burn id/pane/insert **之前**)、覆盖全 create 路径、不误拒 cto、SPA 默认确为 cto(`chatDefaults.ts`,web 无残留 assistant);FIX-3 读末行 ts 鲁棒、真停顿仍 STUCK、status 仍只读。
- **W1 cto 门 deny-by-default / pre-gateway(gateway=None 也拒)/ collect lock-discipline** 正确(缺陷只在 R-M1 socket 对端信任 + R-M3 无 project 维度)。
- **W4**:history 端点修对(真从 turns.jsonl 返回)、sid **无路径穿越**(只作 HashMap key)、前端 per-session 隔离干净(per-sid localStorage、无共享 web-chat socket、切换无串台)。
- **W3 path-traversal 干净**(allowlist 变换 + validator backstop,18 对抗输入 0 逃逸);overwrite 受 `--force` 门控;TLS 开;honest error。
- **W5 auth**:`/api/docs` + `openapi.json` 都在 web-token 门后(401/200 测过);drift test 真。
- **W6**:版本 0.8.7 全 5 site;CLAUDE.md 149 行 + HITL/cto 红线齐;README 英文无版本爬;skill-gate 0;17 工具 enforced;tech-design/usage 齐。
- **HITL 核心** fail-closed 健全、hitl 真去 `--dangerously-skip-permissions`(`claude_tui.rs:322-326`)、IM 审批路完好、mode 穿全 create + 重启持久。

### ship 建议
**v0.8.7 可发(tag + main-merge)——建议 tag 前只先处理 R-H1**(至少 web hitl 按钮诚实化:隐藏/禁用 + 提示去 IM 批,别让"已回应"骗人)。**R-M2**(hitl 复用回执假象)建议顺手——都是 HITL"让人误以为受监督/已批准"的安全语义问题,正是这功能最不该出的错。**R-M1** 的 handoff"不可伪造"措辞建议现在改诚实(便宜)。其余 M/L → v0.8.8。gate + 卫生我已独立复跑确认绿。
