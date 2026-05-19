# V0.5.1 — Sessions visibility(F103)+ Web Topology fixes(F104)

> **立项主线**:V0.5.0 ship 后 host E2E 暴露 — Sessions 数据在后端完全 OK(`active_sessions` API 返 planner aea52061),但 SPA 三处 gap 导致用户**看不见**:
> - 顶级 `/sessions` 导航 tab 是 dead route(`App.tsx:101` 加 tab + `App.tsx:140-146` 无 Route → catch-all PlaceholderPage)
> - WorkflowView agent 卡片默认折叠,`running_count > 0` 没视觉提示
> - SessionDetail 只支持 flex project,workflow project session 链接 404
>
> F103 ship 后第二轮 host E2E 又暴露 — `/teams/<name>` Topology 把 `Explore` 内置 subagent 渲染成 `unknown · haiku` + 误判 `definition_backed: true` → 弹 "definition missing" 警告。

V0.5.1 把"用户从浏览器看到 host 上跑的 planner"端到端走通,并修好 Agent Teams Topology 对 Anthropic 内置 subagent type 的支持。

---

## Finding 列表

| # | 标题 | 一句话 |
|---|---|---|
| **F103a** | 全局 `/sessions` 列表页 + aggregate API | 新 `GET /api/v1/sessions/active` 跨项目聚合 `active_sessions`;新 SPA `SessionsListPage.tsx` 接 `/sessions` 路由,卡片显示 slug + role + cost + model + cwd,点击下钻 SessionDetail |
| **F103b** | WorkflowView agent 卡片 running_count > 0 自动展开 + 显眼徽章 | 初次 fetch active_sessions 后,把第一个 `running_count > 0` 的 role 设为 `expandedRole`;卡片头加 `● <N> running` 红点徽章(目前数字不够显眼)|
| **F103c** | SessionDetail 支持 workflow project | API `/api/v1/projects/<slug>/sessions/<sid>` 当前只接 flex 项目 → 404 for workflow;扩 handler 读 workflow project 的 progress.jsonl + state.json + transcript jsonl,返同形 SessionDetail JSON;SPA `SessionDetail.tsx` 兼容 workflow 项目场景(可能没有 harness;就不渲染 HarnessPanel)|
| **F104a** | `/api/v1/teams/<name>` 序列化 snake_case | `MemberView` / `TeamConfig` 全部字段(`agentId` / `agentType` / `joinedAt` / `tmuxPaneId` / `backendType` / `planModeRequired` / `createdAt` / `leadAgentId` / `leadSessionId`)改为 `serde(alias = "<camelCase>")`,序列化输出 snake_case 供 SPA 消费,反序列化继续接受 Anthropic 的 camelCase 上游 |
| **F104b** | F95 `definition_backed` allowlist 加宽 + 文件存在性回退 | `definition_backed_for` 加入 `Explore` / `explore`(Anthropic 内置 subagent type,没 `.md` 可解析);新 `compute_definition_backed_with_scope` 在 web 侧叠加 scope-chain 文件存在性检查 — 任何 allowlist 之外的 agentType 若在 project / user / plugin / managed scope 都找不到 `.md` 文件,降级为 ad-hoc(防 SPA 报"definition missing")|
| **F104c** | Bug 2 fallout: `/ccteam:team → /ccteam-team` 测试 fixture 修复 | `meta_agent::tests::render_role_prompt_routes_via_v050_skills` + `skill::tests::ccteam_team_skill_installs_under_canonical_dir` 的 expected 字符串同步到当前 skill body |

---

## 双源验证

- **后端 truth**:`ccteam show <slug>` CLI + `GET /api/v1/projects/<slug>/sessions/active` 两者必须显示同一 active session 集合
- **前端 truth**:从 `/sessions` 顶级 tab 或 `/p/<slug>` 项目页都能看到运行中的 session

---

## 验收

1. `ccteam show dex-ui` 有 1 running planner → 浏览器 `/sessions` 出 1 张 planner 卡片(dex-ui 项目)
2. `/p/dex-ui` 打开默认展开 planner 卡片,显示 "Running sessions (1)"
3. planner 卡片头有 `● 1 running` 红点徽章
4. 点 planner session → `/p/dex-ui/s/planner-1779035055413161` 渲染成功(workflow 项目不再 404)
5. 无 running session 时 `/sessions` 显示 "No running sessions. Spawn via `ccteam start <slug>` or drop trigger artifact."
6. `/api/v1/sessions/active` 返回 `Vec<ActiveSessionInfo & { slug: String }>`(每 row 多一个 slug 字段)
7. 测试 baseline ≥ 931/1 + 新增 ~8 tests(4 backend + 4 frontend)

---

## 红线

- **不改 ccteam-core::active_sessions 签名** — F92 / F90 都用它;只在 web crate 加 aggregate handler
- **不动 SessionDetail 已有 flex 路径** — 只扩 workflow 分支
- **WorkflowView 自动展开默认只展开 1 个**(第一个 running_count > 0 的 role)— 多 role 都 running 时不要全展开(屏幕爆)
- **`/sessions` 顶级页 lazy-load** — 跟 `/teams` 一致,React.lazy 单独 chunk

---

## 时间表

| 阶段 | 内容 | 估时 |
|---|---|---|
| 单 subagent (F103) | F103a + b + c | 4-5h |
| 单 subagent (F104) | F104a + b + c(test fixes) | 2-3h |
| 主 session | merge + test + bump 0.5.0→0.5.1 + tag + push | 30min |

总:~7-8h calendar time
