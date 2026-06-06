# v0.8.8 PRD — 独立 session 模型 + 实机 bug 批修

> **状态:DRAFT,需求持续补充中**(user 边实机用边加;本文 §一 范围表 + 对应小节滚动更新)。
> **流程:doc-first** —— 本 PRD(+ scope 冻结后的 dev-plan)是**需求收集 + 文档**产物;**实现是另一个 dev session**,user review 文档后才动代码。本文作者**只收集需求 + 写文档,不开发**。
> **来源**:v0.8.7 ship 后实机(IM + web)讨论,2026-06-06(TG)。
> **代码基线**:dev `e8e26a2`(v0.8.7 + review-fix B/C 已落)。
> **bug 清单 + 根因 file:line**:见同目录 `bug.md`(SoT,持续追加)。

---

## 〇、一句话

v0.8.7 的 per-session web UI 把"会话"做成了 **UI 壳独立、但数据按 `(项目, role)` 共享** —— 因为底层 keystone 是 `session = role`。v0.8.8 把 **session 升成一等实体(独立、有持久 id)**,让多个会话像 Claude Code 原生 session 一样互不串台;附带清掉 v0.8.7 遗留的几个实机 bug + 重写 `ccteam status`。

## 一、范围(初版,持续加)

| # | 项 | 类型 | 一句话 | 来源 |
|---|---|---|---|---|
| **F1** | 独立 session 模型 | 架构(headline) | session 一等实体 + 持久 id;改 `session = role` keystone + 去 dedup | 根治 BUG-3 |
| **F2** | roleless session | feature | role 空 → spawn 不加 `--agent`(裸 claude) | ENH-1 |
| **F3** | `ccteam status` 重写 | CLI/UX | 列全部项目 + 其会话;删最近事件;web token/url 两行(LAN ip) | TG 2363 |
| **B1** | `project stop` 跟 backend 走 | bug | 现 tmux-only,默认 rmux 停不掉 | BUG-1 |
| **B2** | web「新建项目」入口恢复 | bug | v0.8.7 W4 回归删掉了 | BUG-2 |
| **B3** | 新建弹窗 role 改下拉真实 role | bug/UX | 现手输 + 静态建议,不知有哪些 role | BUG-4 |

> BUG-3(per-session 串台)**不单列修项** —— 它是 `session = role` 的症状,由 **F1 根治**。
> 后续 user 追加的需求 append 到本表 + 新小节。

## 二、F1 · 独立 session 模型(headline)

### 2.1 需求(user 原话)
> "聊天独立 = 跟 Claude Code 原生 session 一样,终端起多个会话互不串台,同一个 role 开两个也各聊各的。"

### 2.2 方向(已定 = A;A/B 取舍见 `bug.md` BUG-3)
- **session = 一等实体**,有**持久 id**(不是现在一重启就重置的内存 `s{n}`)。
- **role 降级为 session 的属性**(决定用哪个 `--agent`,或不带 = F2)。
- **去掉 `(项目, role)` dedup**(现"单 (项目,role) 一个 pane")→ 同 role 可并存多个 session,每个 = 自己的原生 claude session(UUID)+ 自己的 pane + 自己的 turns(按 **session id** 存,不是按 role)。
- **历史 + SSE 都按 session id** 走 → 天然不串台。

### 2.3 影响(动到的红线 / blast radius)
- **改 keystone**:`session = role` → `session = 独立会话;role 是 session 的一个属性`。CLAUDE.md §〇/§三 + `tech-design.md` 相关红线要重写。
- **改 invariant**:dedup「单 (项目,role) pane」→ 允许同 role 多 pane。
- **保留** resume-by-id(红线),但 resume key 从 `(项目, role)` 改成 **session id**;"空闲释放 + 扛 daemon 重启"语义不变,只是粒度变细。
- 触及面(非完整设计,只列已知):gateway session 模型 + 持久化(session id 落盘)、tmux/rmux 命名(`ccteam-chat-<slug>-<role>` → 需含 session id)、`turns_mirror`(按 session 存)、`claude_tui` 的 `spec_for_new`/`spec_for_resume`(`--agent` 变可选 + `--name` 每 session 唯一)、`/role`、web/IM 寻址(一个 chat 里多个同 role 会话怎么切)、旧数据迁移。

### 2.4 开放问题(待 user 拍 / 设计深化)
1. **session 持久 id 形态**:ccteam 自己 mint uuid?还是复用 claude 原生 session UUID(注意 `--name` 是 title 非 id,要另持有真 id)?
2. **IM 里怎么寻址多个同 role 会话**:`/new` 给每个 session 一个 handle?`/use <handle>` 切?(现有 `/new /use /cd` 如何扩。)
3. **`/role` 语义**:改当前 session 的 role(原地 re-spawn 换 `--agent`),还是必然开新 session?
4. **roleless 的 turns key / 显示名**(联动 F2)。
5. **web session list**:同 role 多会话怎么展示 + 命名(现 rail 每 `(项目,role)` 一条)。
6. **session 生命周期**:谁能删?空闲释放策略?是否给上限(防一个 role 开爆 pane)?

## 三、F2 · roleless session(ENH-1)
- **需求**:新建 session role 留空 → spawn **不加** `--agent`(裸 claude,brain 走项目 `CLAUDE.md` 原生自读);非空才 `--agent <role>`。
- **依赖 F1**:roleless 没 role 当 turns key → 必须先有 session 持久 id。
- **已知改动**:前端别把空 role 默认成 cto;create 路径"**仅非空 role 才校验存在**"(保留 FIX-2 对非空的校验,防 `--agent <未定义>` 死 pane);`spec_for_new/resume` 支持"空 role 跳过 `--agent`"。
- **红线说明**:roleless ≠ role,是 `session=role` → `session 一等实体` 之后的自然态(session 可以没有 role)。

## 四、F3 · `ccteam status` 重写(TG 2363)
### 4.1 需求(user 原话拆解)
1. **输出所有项目 + 每个项目下面的会话(session)**(嵌套展示,不只项目级)。
2. **删除「recent events (last 5)」段**。
3. **"两行"** —— web 访问信息两行:
   - `web token: <hex>`(裸 hex,不带 `ccteam:` 前缀)
   - `web url: http://<局域网ip>:7331/?token=ccteam:<hex>`(URL 里带 `ccteam:` 前缀;**ip 取局域网 ip**,不是 localhost/0.0.0.0)
### 4.2 现状参考(改之前长这样)
`ccteam status` 现含:daemon health 行 + projects(每项目 age/last-event/STUCK|OK + STUCK 时附 peek/attach 提示)+ recent events(last 5)+ `web token:` 一行(带 `ccteam:` 前缀)。**没有 web url 行**(需新增 + LAN ip 探测)。
### 4.3 开放问题(待 user 确认)
1. **"两行"指什么**:我理解 = web token / web url **各一行、共两行**(下方 user 示例就是这俩);若"两行"另有所指(如整体压到两行 / 每项目两行)请确认。
2. **项目 + 会话的展示密度**:每项目一行、会话缩进列?会话显示什么(session id / role / status / last-event)?(联动 F1 的"会话"新定义。)
3. **STUCK/OK 留不留**:删了 recent events 后,项目级健康(STUCK/OK)是否保留?(注:FIX-3 后 STUCK 已改从 progress.jsonl 末事件取,demo2 silent 1h = 真停顿、非误报。)
4. **LAN ip 探测**:多网卡/容器时取哪个?(默认私网网段第一个?可配?)
### 4.4 归属
CLI 输出层重写(`commands.rs` status handler + `queries.rs` 取数);LAN ip 探测 + web url 拼接是新增;会话列举依赖 F1 的 session 模型(若 F1 未落,先按现 gateway session 列)。

## 五、Bug 修(详见 `bug.md`,各条 file:line 已验证)
- **B1 / BUG-1**:`stop_project_chat_sessions` 改经 `default_backend()` 枚举 + kill(去 tmux-only)。
- **B2 / BUG-2**:web「＋ 新建」恢复「＋ 新建项目…」,走 REST `POST /api/v1/projects`。
- **B3 / BUG-4**:新建弹窗 role 改下拉,拉 `GET /api/v1/projects/{slug}/roles`。
- **BUG-3**:由 **F1 根治**,不单独修。

## 六、流程 & 纪律
- **doc-first**:本 PRD → scope 冻结 → dev-plan(waves)→ user review → **另一个 dev session 实现**。本文作者只收集需求 + 写文档,不开发。
- **需求持续补充**:user 边用边加 → append 到 §一 表 + 新小节;dev-plan 等 scope 稳了再写。
- **pre-v1.0 纪律**:不做历史迁移 —— 新旧 session 数据不兼容时直接「清旧(`~/.ccteam` + 各项目 `.ccteam`)→ 重 `ccteam init`」,不写迁移/兼容分支;deprecated 直接删。

## 七、变更记录
- **2026-06-06 初版**:F1 独立 session(Direction A)+ F2 roleless + B1/B2/B3;BUG-3 归 F1 根治。
- **2026-06-06 +F3**:`ccteam status` 重写(TG 2363:列项目+会话 / 删最近事件 / web token+url 两行 LAN ip)。
