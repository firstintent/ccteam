# v0.8.7 review-fix — dev session briefing(修复完成,不许偷懒)

## 背景
v0.8.7 全 6 wave 已上 `dev`(`44a12c7`)。我(reviewer session)做了 5 路并行对抗复审 + 从冷 target 独立复跑 gate(`1942/0` · clippy 0 · fmt 干净,**属实**)。**无 blocker**,但有 1 HIGH + 7 MEDIUM + 6 LOW + 1 验证缺口。**全部 file:line + 根因 + 修法在 `docs/versions/v0-8-7/fix.md` 末「审查发现」节** —— 起手先读它。本轮目标 = **把这些 finding 全部修复完成**(不是 interim 缓解)。

## 基线 / gate(每个 commit 前必过,只增不减)
- `cargo test --workspace --exclude ccteam-web` ≥ **1942/0**(每修加回归测试)
- `cargo clippy --workspace --all-targets -- -D warnings` = 0
- `cargo fmt --all -- --check` 干净
- `cargo build -p ccteam-web` + `vitest` 过
- `ccteam doctor --verify-mcp` = 17/17、0 drift
- 直接在 `dev` 提交(无 PR),commit 前缀 `v0.8.7-fix:`;改协议同步 `tech-design.md`「协议→代码」表,改红线同步 `CLAUDE.md`(≤200 行)
- 收尾写 `docs/versions/v0-8-7/review-fix-handoff.md`:**每条 finding 标 `fixed`(+ 测试名)或 `deferred`(+ 明确理由 + 计划)**

## 纪律 —— 关键,不许偷懒
1. **R-H1 必须真修**:web HITL Approve 要**真能批准**(点批准→工具真跑;点拒绝→真即时拒),**不是**隐藏按钮的 interim。
2. **每条 finding 要么修完、要么 handoff 显式写明为何 defer + 计划** —— 不许静默跳过。
3. **每个修加回归测试**钉住(尤其 R-H1 的 web approve round-trip、R-M2 receipt、R-M3 跨项目拒、R-M4 purge)。
4. **R-M1 别再把门称"不可伪造"**:单 OS-uid 模型下 agent 之间无硬边界(同 uid 可读他进程 `/proc/<pid>/environ`、文件、ptrace),secret-token 只**抬高门槛**(defense-in-depth),**不是 close**。诚实写明真隔离 = 未来 per-agent OS user / sandbox(v0.8.8+)。**不许加完 token 又改口说"现在不可伪造"** —— 那是我复审里专门点的过度声称。
5. 验收里写的测试都要真加、真过。

---

## 必修:HIGH + 全 MEDIUM

### R-H1 [HIGH] web HITL 批准真正可用(token-resolve,**非**隐藏按钮)— tag 前置
- 现状:`ChatConsole.tsx:181-198` `resolveApproval` 把 `String(idx+1)` 当**普通 turn** POST → `handle_session_turn`(`sessions_api.rs:326-356`)直进 `submit_to_sid`,**绕过** `resolve_selection`/`resolve_numeric` → token-keyed pending 永不 resolve → hook 600s 超时 deny;UI 却显示"已回应"。
- 真修(三步):
  1. **SSE approval 事件带 token + option id**:现在 `sessions_api.rs:501-503` 只发 `o.label`,把 `{token}` + 每个 option 的 id(`allow`/`deny`)一并带给前端。
  2. **加 `POST /api/v1/sessions/{sid}/resolve {token, selection}`**:走 IM 同一条 `Gateway::resolve_selection`/`take_by_token` 路解析 pending(**不是**发 turn)。`#[utoipa::path]` 标注 + 进 W5 drift test 期望集(28→29 ops)。
  3. `resolveApproval` 改调该端点(token + option id),删 `submitTurn(String(idx+1))`。
- 验收:web 建的 hitl session,点[批准]→工具**真跑**(非 600s 超时);点[拒绝]→**即时** deny;IM 审批路不回归。**加 web 集成/e2e 测试钉住 approve+deny round-trip**。

### R-M1 [MED·安全] cto 门抬门槛 + 文档诚实化(两件都要)
- 现状:门信任 `args["_caller_role"]`(`main.rs:2939-2960`);anti-spoof 只在 stdio forwarder 成立(`mcp_session_tools.rs:180-185`);socket 无 peer 校验。
- 真修:
  1. **抬门槛**:spawn 给 pane 注入 per-session secret(随 `CCTEAM_CHAT_ROLE`);daemon 存 `sid→{role,secret}`;forwarder 转发 secret;门校验 `secret↔role` 而非信明文 role。
  2. **诚实化文档**:`wave-1-handoff.md` + `CLAUDE.md` §三 cto 红线行,**删除"agent 不能伪造身份 / daemon 门是稳边界"** 的措辞,改成:单 uid 全信任模型下门是 best-effort defense-in-depth(同 uid 进程可读他 pane 的 env,**非硬边界**);真隔离 = per-agent OS user / sandbox(记 v0.8.8 deferred)。
- 验收:门校验 secret;非 cto / 无 secret → 拒(测试);仓内 grep 无"不可伪造/cannot spoof/稳边界"残留;handoff 有架构 deferred 说明。

### R-M2 [MED] `/new … hitl` 命中已存在 skip pane 的回执假象(危险方向:以为受监督其实没)
- 现状:`gateway.rs:799-804` 按**请求** mode 拼回执,但 `:1010-1022` dedup 复用旧 session **忽略**请求 mode → 回执报 hitl,pane 还在 skip 裸跑。
- 真修:`start_session` 复用时返回**实际** mode;`/new` 据实际拼回执(命中 skip pane 时明确 `reusing s{n}(仍 skip,停掉重建才能 hitl)`)。验收:skip 的 cto 上 `/new claude cto hitl` → 回执不谎称 hitl;加测试。

### R-M3 [MED] cto 门加 project 维度(防跨项目操控)
- `session_spawn` 拒 `project != _caller_slug`(或删 informational 的 project 参数);`dispatch`/`collect`/`stop` 先校验 `session_resolve(sid).project == _caller_slug`(`main.rs:3017-3028 / 3060 / 3088-3097 / 3187-3189`)。验收:A 项目 cto 操 B 项目 sid → 拒;加测试。

### R-M4 [MED] `project rm --purge` 清 `PermissionRequest` hook(purge = init 逆,红线)
- `tool_surface.rs:880-890` `hook_command_is_chat_hook` 谓词加 `permission-request` 匹配。验收:种了 PermissionRequest 的项目 `--purge` 后 `settings.local.json` 无残留;加回归。

### R-M5 [MED] 自托管 Scalar(去 `cdn.jsdelivr.net`,产品主打自托管/锁网机)
- `openapi.rs:123`:`.custom_html(include_str!(...))` + vendored Scalar standalone JS 走 `/assets`(或至少 pin 精确版本 + SRI)。验收:离线/防火墙后 `/api/docs` 能渲染;加测试断言返回 HTML 不含外部 CDN script(或含 pinned+integrity)。

### R-M6 [MED→LOW] 坏 role 返 4xx 非 500
- `sessions_api.rs:217-221`:区分 persona-missing(→400/422 + 清晰错误)与真内部错(→500)。验收:POST 不存在的 role → 4xx;加测试。

### R-M7 [MED→LOW] `ccteam status`/dashboard 不再全量读 progress.jsonl
- `progress.rs:52` `last_event` 改 **tail-read 末行**(seek EOF 反向读有界块)而非 `read_to_string` 整文件;它落在 `collect_projects()` 每次 status/ls/`GET /api/v1/projects`。验收:大 progress.jsonl 下 `ccteam status` 不 O(整文件);加大文件测试。

---

## 也修:LOW(便宜且明确)
- **R-L4** role import body 加 ~1MiB cap(`role_import.rs:162`,streaming + 上限)。
- **R-L5** role import `.redirect(reqwest::redirect::Policy::none())` 或限同 host(`role_import.rs:140-149`)。
- **R-L6** pin `AGENCY_RAW_BASE` 到 sweep commit sha(`role_catalog.rs:53`)+ `role add` 装完提示「用前 review 该 .md(第三方内容)」。
- **R-L1** hitl permission prompt 用更短 TTL(vs 600s 的 interaction/ask)+ outstanding 时发 progress 行,让 operator 知道 agent 被 parked(`main.rs:2545-2549`)。

## 判断后再定:arguable(defer 必须 handoff 写明理由,不许静默跳)
- **R-L2** 从没聊过的项目仍显示 STUCK(`queries.rs:255-257` 保留 `unwrap_or(age)`):可能是合意(idle 该提示)。若改:init 种 baseline event 或 `never-started` 单独判定;否则 handoff 写「按设计保留 + 理由」。
- **R-L3** `session_collect` >20 turn 爆发 cursor 跳过中间(`main.rs:3137-3143`):polled MVP 边界。若改:截断时 cursor 设边界 / 回 `truncated:true`;否则 handoff 写「MVP 限制 + 计划」。
- **nit**:token 40-bit 碰撞(pre-existing D6)、role stem 跨 division 碰撞 —— 低优,记 handoff。

## 验证缺口(必做)
- **PermissionRequest `behavior:allow|deny` 契约对真 binary 验证**:跑 `cargo test -p ccteam-harness --test claude_agent_smoke_test -- --ignored`,**扩断言**:deny 时受害文件**未被删**、allow 时**被删**(`claude_agent_smoke_test.rs:458-528`)。若无真 claude 环境,handoff 写明「未验证 + 风险」(HITL 的安全性整条系于此契约)。

---

## 建议分组(便于 workflow 并行)
- **A·HITL 正确性+安全**:R-H1 · R-M2 · R-M4 · R-L1
- **B·cto 门安全**:R-M1(token + 诚实文档)· R-M3
- **C·web/api/docs**:R-M5(Scalar 自托管)· R-M6(4xx)
- **D·status 性能**:R-M7
- **E·role-import 加固**:R-L4 · R-L5 · R-L6
- **F·验证+arguable**:PermissionRequest 真 binary smoke · R-L2 · R-L3 · nits

## 收尾
全 gate 过 → `review-fix-handoff.md`(逐条 fixed/deferred+理由 + 测试名)→ 报告。**R-H1 修完是 tag + main-merge 的前置**(user 持 tag/merge,修完由 user 放行)。
