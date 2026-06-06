# v0.8.7 review-fix Handoff(逐条 fixed/deferred + 测试)

> 对应 `review-fix-prompt.md`(reviewer 5 路对抗复审:1 HIGH + 7 MED + 6 LOW + 1 验证缺口)+ `fix.md`「审查发现」。
> 3 批 `v0.8.7-fix:` 直接上 dev:**A** `5ced845`(HITL 正确性)· **B** `caefc20→64b6ecb`(cto 门安全)· **C**(本批,hardening + 验证)。
> **最终 gate**:`cargo test --workspace --exclude ccteam-web` = **1975/0**(review-fix 起点 1942 → +33)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `cargo build -p ccteam-web` OK + **vitest 111** · `cargo test -p ccteam-web` 仅 5 个 env-gated `ws_*` PTY 失败 · `doctor --verify-mcp` 17/0 · `/api/docs` 0 外部 CDN。
> **每个 must-fix(HIGH + 全 MED)= fixed**;仅 R-L2 + 2 nits = deferred(显式理由)。**R-H1(tag 前置)真修 + HITL allow/deny 契约真机实证通过**。

## 逐条

| ID | 严重度 | 状态 | 修法 / 理由 | 测试 |
|---|---|---|---|---|
| **R-H1** | HIGH | ✅ fixed (A) | web [批准]/[拒绝] 走真 token-resolve:SSE 带 `{token}`+option `{label,id}`;新 `POST /api/v1/sessions/{sid}/resolve` → `Gateway::resolve_web_selection` → `take_by_token`+`apply_pending`(External `reply.send`)→ 解阻 hook → `{behavior}`;**非** turn。删旧 `submitTurn(idx+1)`。openapi 28→29。 | `web_resolve_{approve,deny,unknown_token}`、`session_event_carries_token_and_option_ids_for_approval` |
| **R-M1** | MED·安全 | ✅ fixed (B) | per-session secret(`ccteam_core::session_secret`,128-bit getrandom,常量时比较)注入 pane env;daemon 存 `sid→{role,secret}`+持久;`verify_session_caller(role,secret)` 校验**对**而非明文 role。**+ 诚实化**:scrub 全部"不可伪造/cannot spoof/稳边界/硬门"硬保证 → best-effort defense-in-depth(单 uid 下同 uid 可读 pane env 拿 secret,只抬门槛非闭合)+ 真隔离 = per-agent OS user/sandbox(v0.8.8)。**未再声称 unforgeable**。 | secret gate allow-cto/reject-nonsecret 单测;`overclaim_residue==0` grep gate |
| **R-M2** | MED | ✅ fixed (A) | `start_session` 返 `StartOutcome{id,permission_mode,reused}`;复用回实际 mode;`/new` 回执据实际拼(复用 skip pane + 请 hitl → "仍 skip,需 stop 重建",绝不谎称 hitl)。 | `gateway_dedupes_sessions_by_project_and_role`(回执断言) |
| **R-M3** | MED | ✅ fixed (B) | `session_spawn` 删 informational `project` 参(只在 caller slug 建);`dispatch/collect/stop` 经 `assert_caller_owns_session`(`session_resolve(sid).project==caller_slug`)拒跨项目 sid。 | 跨项目 reject 单测 |
| **R-M4** | MED | ✅ fixed (C) | `tool_surface::hook_command_is_chat_hook` 加配 `permission-request` → `project rm --purge` 清 HITL hook(purge=init 逆 红线)。 | `remove_chat_hooks_strips_permission_request_keeps_operator`、`remove_chat_hooks_now_empty_includes_permission_request`、`t03b2_purge_strips_hitl_permission_request_hook` |
| **R-M5** | MED | ✅ fixed (C) | **自托管 Scalar**:vendored `@scalar/api-reference@1.58.0` standalone(`crates/ccteam-web/assets/scalar-standalone.js`,3.56MB,sha256 539bbbe6…)经新同源路由 `GET /api/docs/scalar-standalone.js`(web-token 门内);`custom_html` loader 指本地 —— **零 cdn.jsdelivr.net**,离线/锁网可渲染。`SCALAR_VERSION` const = 刷新 chore SoT。 | `docs_html_is_self_hosted_and_version_pinned`、`openapi_json_and_docs_served_under_auth`(扩:无 CDN + vendored JS route 401/200) |
| **R-M6** | MED→LOW | ✅ fixed (C) | typed `gateway::RoleNotFound` → web `handle_create_session` downcast → **422**(清晰 hint);真内部错仍 500;utoipa 标 422。 | `create_session_unknown_role_is_role_not_found` |
| **R-M7** | MED→LOW | ✅ fixed (C) | `progress::last_event` 改 **tail-read**(seek EOF、8KiB 块反向读到末行),O(末记录)非 O(file);命中每 status/ls/`GET /projects`。 | `last_event_tail_reads_large_file_correctly_and_bounded`(256KiB)+ chunk-boundary/basic/absent/round-trip 共 5 |
| **R-L1** | LOW | ✅ fixed (A) | hitl permission prompt 独立**短 TTL**(120s 默认,env `CCTEAM_PERMISSION_PROMPT_TTL_SECS`,vs 600s interaction/ask)+ outstanding 时发 `progress.jsonl` "parked" 行;fail-safe deny 不变。 | TTL const + outstanding-progress 单测 |
| **R-L3** | LOW | ✅ fixed (C) | `session_collect` >n 爆发不再丢中段:`page_collected_turns` 返**最旧 n** 未读 + cursor 推到页边界 + `truncated:true` → 多次 poll 全量有序无损。 | `page_collected_turns_pages_a_burst_without_loss`(25 turn/页 10/3 poll)、`_short_and_unknown_cursor` |
| **R-L4** | LOW | ✅ fixed (C) | role import body **1MiB cap**(Content-Length 早拒 + streaming 超限即 bail,`ImportError::TooLarge`)。 | `import_rejects_oversize_body` |
| **R-L5** | LOW | ✅ fixed (C) | role import `redirect::Policy::none()` → 3xx 变 `BadStatus`(不跟随跨 host 重定向)。 | `import_does_not_follow_redirects`(302→evil → BadStatus,不落盘) |
| **R-L6** | LOW | ✅ fixed (C) | `AGENCY_RAW_BASE` 由 `/HEAD` pin 到全 sha `cf6059d030…`;`role add` 装完提示"第三方 .md,用前 review"。 | `raw_url` roundtrip(value-agnostic,绿) |
| **验证缺口** | — | ✅ **verified (C)** | 扩 `#[ignore]` smoke 断言 allow/deny **契约**,并**真机跑**(real claude 2.1.167 + tmux):**PASSED 22.25s** —— DENY→受害文件**未删**,ALLOW→**已删**。`~/.claude.json` 跑前备份、跑后还原到 pre-run sha(claude 运行中改了它)。HITL 安全(deny 挡/allow 跑)**不再只是"hook fires",已对真 binary 实证**。 | `claude_agent_hitl_permission_decision_contract_smoke`(`#[ignore]`,已 RAN) |
| **R-L2** | LOW·arguable | ⏸ **deferred** | 没聊过的项目仍显 STUCK。**理由**:never-started 项目确实 idle,STUCK-on-idle 是合理 operator 信号(reviewer 亦标"may be intended")。干净修 = 新 `StallLevel::NeverStarted` taxonomy,波及 `as_str`+全 match+CLI 文案+web dashboard+orchestrator escalation,属横切,应独立改;init 种 baseline event 会把生命周期事件混进 chat-turn 流。→ v0.8.8 专项。 | — |
| nit token-40bit | nit | ⏸ deferred | pre-existing D6 token 同窗 40-bit 碰撞;低危,记录。 | — |
| nit role-stem 跨 division 碰撞 | nit | ⏸ deferred | 已 `--force` gated + 提示;低危,记录。 | — |

## Risks / 注意
- **vendored Scalar JS = 3.56MB 入库**(R-M5):换掉 CDN 运行期依赖的代价;若在意仓体积,可改 pin+SRI(briefing 的"至少"备选)。刷新 = bump `SCALAR_VERSION` + 替文件。
- R-M6 web 422 是 thin downcast(无 gateway-backed web 测试 harness;gateway 侧 `RoleNotFound` 有单测)。建 harness = follow-up。
- HITL 真隔离仍是 v0.8.8(per-agent OS user/sandbox);secret 只抬门槛(已诚实记录,见 R-M1 + CLAUDE.md §三 + tech-design §6.4)。

## Files(C 批)
ccteam-core:`tool_surface.rs`、`progress.rs`、`role_catalog.rs`。ccteam-im:`gateway.rs`、`role_import.rs`(+test)。ccteam-web:`routes/openapi.rs`、`routes/sessions_api.rs`、`tests/openapi_test.rs`、`assets/scalar-standalone.js`(新)。ccteam-cli:`commands.rs`、`main.rs`、`tests/remove_test.rs`。ccteam-harness:`tests/claude_agent_smoke_test.rs`。docs:`tech-design.md`。(A/B 批 files 见各自 commit。)

## tag / main-merge
**HOLD**(ship-flow)—— R-H1 前置已满足(真修 + 契约实证);tag `v0.8.7` + main-merge 待 user 放行。
