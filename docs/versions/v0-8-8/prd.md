# v0.8.8 PRD — 独立 session 模型 + web 能力补全 + 实机 bug 批修

> **状态:DRAFT,需求持续补充中**(user 边实机用边加;§二 功能项 + §三 Bug 滚动追加,**新需求只 append `### F*` / `### B*` 子节,不动顶层编号**)。
> **流程:doc-first** —— 本 PRD(+ scope 冻结后的 dev-plan)是**需求收集 + 文档**产物;**实现是另一个 dev session**,user review 文档后才动代码。本文作者**只收集需求 + 写文档,不开发**。
> **来源**:v0.8.7 ship 后实机(IM + web)讨论,2026-06-06(TG)。
> **代码基线**:dev `68e73d8`(v0.8.7 + review-fix B/C 已落)。
> **bug 清单 + 根因 file:line**:见同目录 `bug.md`(SoT,持续追加)。

---

## 〇、一句话

v0.8.7 的 per-session web UI 把"会话"做成了 **UI 壳独立、数据按 `(项目, role)` 共享**(因为 keystone 是 `session = role`)。v0.8.8:(1)把 **session 升成一等实体(独立、有持久 id)**,让多会话像 Claude Code 原生 session 一样互不串台;(2)**补全 web 能力**(config 配置模块 + role 浏览页 + status 重写 + 终端修复);(3)清掉 v0.8.7 遗留的实机 bug。

## 一、范围(初版,持续加)

| # | 项 | 类型 | 一句话 | 来源 |
|---|---|---|---|---|
| **F1** | 独立 session 模型 | 架构(headline) | session 一等实体 + 持久 id;改 `session = role` keystone + 去 dedup | 根治 BUG-3 |
| **F2** | roleless session | feature | role 空 → spawn 不加 `--agent`(裸 claude) | ENH-1 |
| **F3** | `ccteam status` 重写 | CLI/UX | 列全部项目 + 其会话(含 vendor);删最近事件;web token/url 两行(LAN ip) | TG 2363/2366 |
| **F4** | web config 配置模块 | feature | web 上配 IM,重点 Telegram + Lark | TG 2370 |
| **F5** | web role 浏览页面 | feature | web 浏览(已装 role,可选 catalog) | TG 2371 |
| **B1** | `project stop` 跟 backend 走 | bug | 现 tmux-only,默认 rmux 停不掉 | BUG-1 |
| **B2** | web「新建项目」入口恢复 | bug | v0.8.7 W4 回归删掉了 | BUG-2 |
| **B3** | 新建弹窗 role 改下拉真实 role | bug/UX | 现手输 + 静态建议,不知有哪些 role | BUG-4 |
| **B4** | `session ls`/`status` 显示 vendor + 修 codex 活性误报 | bug/UX | session ls 按 backend 名枚举、看不到 codex | BUG-5 |
| **B5** | web 终端 PTY WS 修复 + "像本地终端" | bug/feature | 连上即断:路由没指向会话 pane + I/O 硬编码 tmux | BUG-6 |

> BUG-3(per-session 串台)**不单列修项** —— 它是 `session = role` 的症状,由 **F1 根治**。

---

## 二、web UI 质量基线(横切要求,所有 web 项 F1(前端)/F4/F5 + B2/B3/B5 必须满足)

> **user(TG 2373):"web 类开发增加要求,必须设计高质量、交互友好的 UI。"** 下列是把它落成**可验收**的标准,不是泛泛"做好看点"。

- **一致设计系统**:复用现有 Tailwind 主题 token(`surface-*` / amber 强调色)+ 既有组件风格(`crates/ccteam-web/web/src/components/`);不引入风格割裂的新控件。
- **完整状态**:每个数据视图都有 **loading / empty / error / success** 四态,不留白屏 / 不卡死;异步操作即时反馈(按钮 disable + spinner / 乐观更新)。
- **错误可读**:API 错误转人话提示(不是裸 500 / stack),可重试。
- **响应式 + 移动友好**:SPA 已含移动键盘 / 手势 hook(`useMobileKeyboard` / `useEdgeSwipe` / pinch-zoom),新页在窄屏可用。
- **交互细节**:键盘可达(focus、Esc 关弹窗、Enter 提交)、防重复提交、危险操作二次确认、长列表可滚 / 必要时虚拟化。
- **即时性**:列表 / 状态走现有 SSE / 轮询保持新鲜,避免"手动刷新才更新"。
- **无障碍基线**:语义化标签 + 对比度 + 可聚焦。
- **✅ 验收追加**:每个 web 项的验收除"功能对"外,**加一条"UX 过关"**(上述清单 + 人工走查);dev-plan 里 web wave 的 gate 含一轮 UI review。

---

## 三、功能项

### F1 · 独立 session 模型(headline)

**需求(user 原话)**:"聊天独立 = 跟 Claude Code 原生 session 一样,终端起多个会话互不串台,同一个 role 开两个也各聊各的。"

**方向(已定 = A;A/B 取舍见 `bug.md` BUG-3)**:
- **session = 一等实体**,有**持久 id**(不是现在一重启就重置的内存 `s{n}`)。
- **role 降级为 session 的属性**(决定用哪个 `--agent`,或不带 = F2)。
- **去掉 `(项目, role)` dedup** → 同 role 可并存多个 session,每个 = 自己的原生 claude session(UUID)+ 自己的 pane + 自己的 turns(按 **session id** 存)。
- **历史 + SSE + 终端 + session ls/status 都按 session id** 走 → 天然不串台,且为 B4/B5 提供统一的"按 session 取数"。

**影响(动到的红线 / blast radius)**:
- **改 keystone** `session = role` → `session = 独立会话;role 是属性`(CLAUDE.md §〇/§三 + `tech-design.md` 重写);**改 invariant**「单 (项目,role) pane」→ 允许同 role 多 pane;**保留** resume-by-id 但 key 从 `(项目,role)` 改成 **session id**。
- 触及:gateway session 模型 + 持久化、tmux/rmux 命名(含 session id)、`turns_mirror`(按 session 存)、`claude_tui` `spec_for_new/resume`、`/role`、web/IM 寻址、`pty_ws`(B5)、`session ls`(B4)、旧数据迁移。

**开放问题(待 user / 设计深化)**:① session 持久 id 形态(mint uuid vs 复用 claude 原生 UUID;注意 `--name` 是 title 非 id);② IM 怎么寻址同 role 多会话(`/new` 给 handle?`/use`?);③ `/role` 语义(改属性 vs 必开新);④ roleless 的 turns key / 显示名;⑤ web session list 同 role 多会话展示;⑥ session 生命周期(删/空闲释放/上限)。

### F2 · roleless session(ENH-1)
- **需求**:新建 session role 留空 → spawn **不加** `--agent`(裸 claude,brain 走项目 `CLAUDE.md`);非空才 `--agent <role>`。
- **依赖 F1**(roleless 无 role 当 turns key → 需 session 持久 id)。
- **已知改动**:前端别把空 role 默认成 cto;create 路径"仅非空 role 才校验存在"(保留 FIX-2 守);`spec_for_new/resume` 空 role 跳过 `--agent`。
- **红线说明**:roleless ≠ role,是 `session 一等实体` 后的自然态。

### F3 · `ccteam status` 重写(TG 2363)
**需求**:① 列**所有项目 + 各自的 sessions**(嵌套);② **删「recent events (last 5)」**;③ web 访问**两行**:`web token: <hex>`(裸 hex)+ `web url: http://<局域网ip>:7331/?token=ccteam:<hex>`(url 带 `ccteam:` 前缀、**LAN ip**);④ **每个会话行显示 vendor**(claude/codex;status + session ls 都加,TG 2365)。
**现状**:现 status = daemon health + 项目级(age/last-event/STUCK|OK)+ recent events + 单行 `web token:`(带前缀)、**无 web url**。
**已定(TG 2366)**:"两行"严格按 user 实例;status 同显所有 projects + sessions。
**开放(待 user)**:会话行除 vendor 外还显示啥(role/status/sid/last-event,建议都要);STUCK/OK 留不留(FIX-3 后是真停顿、非误报);LAN ip 多网卡怎么取。
**归属**:CLI 输出层(`commands.rs` status + `queries.rs` 取数)+ LAN ip 探测 + web url 拼接;会话列举依赖 F1(未落则按现 gateway session 列)。

### F4 · web config 配置模块(TG 2370)
- **需求**:web 上新增 config 配置模块,**重点配 Telegram + Lark**。
- **现状**:配置能力在 CLI `ccteam config`(setup hub);IM creds 存 `crates/ccteam-im/src/credentials.rs` 的 `Credentials`(telegram/lark/slack/discord)→ JSON 文件 **mode 0600**。`TelegramCreds`(bot_token + chat_id),`LarkCreds`(app_id + app_secret + allowed_user_ids + `use_feishu` 选区 CN/intl)。校验器:`run_config_set_im_token`(telegram:验 token + 抓 chat_id,**交互式**)、`run_config_set_lark_creds`(lark:拉 WS endpoint 验,非交互)。**web 侧目前无 IM config 路由**。
- **设计方向**:新增 web 设置页 + API(GET 状态 / PUT 写,**web-token 门后**),复用 CLI 校验逻辑(别重写验证)。
- **🔒 安全(必须处理)**:creds 是 0600 秘密;web bind 默认 `0.0.0.0:7331`(LAN 可达)、**默认无 TLS** → 秘密走 LAN 明文有风险。要求:① 读取**绝不回显明文**(只给 masked / "已配置"状态 + 末几位);② 维持 web-token 门;③ 建议上 TLS,或至少文档明确"LAN 明文,慎用公网"。
- **开放问题**:① Telegram 的 chat_id 抓取是**交互式**(要用户去 DM bot)——web 不能阻塞,需异步 UX(填 token → "现在去给 bot 发条消息" → 轮询抓 chat_id);Lark 非交互、直接可做。② 配完热生效(daemon reload creds)还是要重启?③ 范围:只 telegram+lark,还是顺带 slack/discord + 其它 prefs(MCP install / web-token 轮换)?

### F5 · web role 浏览页面(TG 2371)
- **需求**:web 新增一个 role 浏览页面。
- **现状**:项目 role API **已有** —— `GET /api/v1/projects/{slug}/roles`(列表 `[{role,description,model}]`)、`GET …/roles/{role}`(详情 frontmatter+body)、`PUT …/roles/{role}`(写)。**agency-agents catalog(192)的 web 路由不存在**(只有 CLI `ccteam role search/add/list`;v0.8.7 把 catalog web API 列为 follow-on、未做)。
- **设计方向**:(a) 浏览**已装**项目 role(列表 + 详情查看,API 已具备 → 纯前端页);(b) 可选:浏览 + 从 catalog 一键装(需**新增** web 路由 `GET /catalog/roles` + import,= v0.8.7 未做的 follow-on)。
- **开放问题**:① "浏览"范围 = 仅已装,还是也含 catalog 浏览/装?② web 上 role 只读还是可编辑(PUT 已有)?③ 跟 F4 config、新建弹窗 role 下拉(B3)是否统一进一个"设置/角色"导航区?

---

## 四、Bug 修(详见 `bug.md`,各条 file:line 已验证)
- **B1 / BUG-1**:`stop_project_chat_sessions` 改经 `default_backend()` 枚举 + kill(去 tmux-only)。
- **B2 / BUG-2**:web「＋ 新建」恢复「＋ 新建项目…」,走 REST `POST /api/v1/projects`。
- **B3 / BUG-4**:新建弹窗 role 改下拉,拉 `GET /api/v1/projects/{slug}/roles`。
- **B4 / BUG-5**:`session ls` 活性 + vendor 从 gateway session map 取(修 codex 误报"not running")+ 加 vendor 列;与 F3 同源。
- **B5 / BUG-6**:web 终端 per-session PTY WS 按 sid 解析到对的 pane(修 W4 遗留 TODO)+ `send_keys/resize` 改 `default_backend()`;"像本地终端"完整保真需 rmux 裸字节(W2b)。
- **BUG-3**:由 **F1 根治**,不单独修。

> **同类模式**:B1 / B4 / B5(+ BUG-3)都是 **tmux 硬编码 / 按 role 而非 session** 在默认 rmux + per-session 下露馅;**F1(gateway = session SoT)是它们的结构性修复主干**。

## 五、流程 & 纪律
- **doc-first**:本 PRD → scope 冻结 → dev-plan(waves)→ user review → **另一个 dev session 实现**。本文作者只收集需求 + 写文档,**不开发**(TG 2362)。
- **需求持续补充**:user 边用边加 → append `### F*` / `### B*` 子节 + §一 表行;dev-plan 等 scope 稳了再写。
- **pre-v1.0 纪律**:不做历史迁移 —— 新旧 session 数据不兼容时直接「清旧(`~/.ccteam` + 各项目 `.ccteam`)→ 重 `ccteam init`」,不写迁移/兼容分支;deprecated 直接删。

## 六、变更记录
- **2026-06-06 初版**:F1 独立 session(Direction A)+ F2 roleless + B1/B2/B3;BUG-3 归 F1 根治。
- **2026-06-06 +F3 / +B4 / +B5**:F3 status 重写(TG 2363,开放问题经 2366 确认、加 vendor);B4 session ls vendor + codex 活性(BUG-5,TG 2365);B5 web 终端 PTY WS 断开(BUG-6,TG 2367/2368)。
- **2026-06-06 +F4 / +F5 + 结构重排**:F4 web config 模块(telegram+lark,TG 2370);F5 web role 浏览页(TG 2371);PRD 改成"功能项 = `### F*` 子节"的可扩展结构(新需求不再动顶层编号)。
- **2026-06-06 + web UI 质量基线**:新增横切要求 §二(TG 2373:高质量、交互友好 UI),落成可验收清单(设计系统 / 四态 / 错误可读 / 响应式 / 交互细节 / 即时性 / a11y),每个 web 项验收加"UX 过关"+ web wave gate 含 UI review。
