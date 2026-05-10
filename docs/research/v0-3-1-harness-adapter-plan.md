# V0.3.1 — HarnessAdapter 抽象层(临时计划记录)

> 这是 V0.3 ship 前的**临时占位记录**,捕获用户与 dispatcher session 在
> 2026-05-10 Telegram 上敲定的 V0.3.1 patch 方向,免得 M5.4 ship gate 跑
> 完后失忆。V0.3 ship 后正式起 `docs/v0-3-1/{prd.md,dev-plan.md,README.md}`,
> 本文届时折进或归档。

---

## 1. 触发

V0.3 web UI 派完 4/5 PR(M5.0-M5.3)、M5.4 ship gate 跑到一半时,用户在
Telegram(`message_id 307`)指出:Claude Code 的 statusline + subagent 面板
里**有大量结构化数据**(模型名 / context 用量 / token 计数 / subagent 列表
+ 进度),V0.3 dashboard **拿不到**结构化版本(只有截图视觉里的像素文本),
建议加进 V0.3.1 并**封装成 harness adapter** — Claude Code 是第一个适配,
codex 是第二个(用户 Telegram message 309 + 已有 `docs/research/ccteam-codex-integration.md`)。

---

## 2. 范围 — 不进 V0.3,进 V0.3.1

**V0.3 不动**(M5.4 ship gate 节奏不破):
- web UI 现有数据源 = `progress.jsonl` 事件 + tmux pane 截图(F38)
- screenshot 视觉上**已经包含** statusline + subagent 面板的像素;但不可 query

**V0.3.1 进入 scope**:
- 新增 `ccteam-core::harness` 模块 + `HarnessAdapter` trait
- `ClaudeCodeAdapter` 实现:dual-write `statusline-command.sh` 的 stdin JSON
  到 `~/.ccteam/harness/<slug>.json`(slug 从 `cwd` 推导);加 `notify` watch +
  SSE endpoint 让 dashboard 渲染 model / context% / cost / rate-limit / 历史 token
- subagent live 列表:**部分覆盖** — `progress.jsonl` 已记录 `SubagentStart` /
  `SubagentStop` 事件(粗粒度 start/stop,无 live progress)。Claude Code 不
  暴露 in-flight subagent 进度,这块只能截图 fallback,直到上游加 first-class API
- `CodexAdapter`(占位):放在 trait 后面预留实现,由 `ccteam-codex-integration.md`
  研究 doc 落实。V0.3.1 第一版只 ship Claude Code 适配 + trait 抽象;Codex
  适配可能 V0.3.1 落 stub、V0.3.2 / V0.4 落实

---

## 3. 架构骨架(initial design,V0.3.1 PRD 时跟用户 review 后定稿)

### 3.1 Trait 定义

```rust
// crates/ccteam-core/src/harness.rs

pub trait HarnessAdapter: Send + Sync {
    /// Stable name, e.g. "claude-code", "codex".
    fn name(&self) -> &'static str;

    /// Ingest a fresh status snapshot from the harness's native channel
    /// (Claude Code statusline stdin JSON; codex's equivalent TBD).
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot>;

    /// Best-effort subagent state. Returns empty Vec when the harness
    /// doesn't expose this surface (V0.3.1 Claude Code: empty until
    /// upstream API; V0.4+ codex if available).
    fn subagent_states(&self, snapshot: &HarnessSnapshot) -> Vec<SubagentState> {
        vec![]
    }
}

pub struct HarnessSnapshot {
    pub model_display_name: String,
    pub context_used_pct: u8,             // 0-100
    pub cost_usd_total: f64,
    pub rate_limit_pct: Option<u8>,
    pub cwd: Option<PathBuf>,
    pub raw: serde_json::Value,           // full JSON for forward-compat
    pub captured_at: DateTime<Utc>,
}

pub struct SubagentState {
    pub kind: String,                     // "main", "general-purpose", "code-reviewer"
    pub label: Option<String>,            // "V0.3 PR #5 M5.4 ship gate"
    pub running_for: Option<Duration>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
}
```

### 3.2 Data flow

```
Claude Code TUI
  ↓ stdin JSON
statusline-command.sh (tee'd)
  ↓ original render path                  ↓ NEW dual-write
TUI footer string                        ~/.ccteam/harness/<slug>.json
                                          ↓ notify watch
                                         ccteam-web::routes::sse::handle_harness_sse
                                          ↓ SSE (event: harness_snapshot)
                                         dashboard project.html panel
```

slug 推导:从 stdin JSON 的 `.cwd` 字段匹配 `~/projects/<team>-<slug>/`
路径前缀。匹配不到的 session(meta-agent / random claude session)写
`~/.ccteam/harness/_meta-<handle>.json` 或丢弃。

### 3.3 ClaudeCodeAdapter 实现要点

- `cct doctor --install-statusline-adapter`(or `ccteam doctor --...`)
  写一份 wrapper `~/.claude/statusline-command.sh`(检测用户原脚本,
  保留原逻辑 + 加 dual-write),用 marker `# ccteam-managed:statusline begin/end`
  保护用户手改段(同 V0.2.2 F39 marker pattern)
- 用户已有自定义 statusline 脚本 → wrapper `tee` 进 `~/.ccteam/harness/`
  + 透传 stdin 给原脚本;无脚本 → 直接 dual-write
- 不解析 statusline 渲染输出,只解析 stdin JSON(CLAUDE.md §三 SoT 红线
  — progress.jsonl 仍是 orchestrator 的事实来源,harness snapshot 是
  **presentation 信息**,不参与状态判定)

### 3.4 Web 层

- 新 route `/sse/harness/<slug>` — SSE 推 harness_snapshot 事件
- `templates/project.html` 加 `Harness` panel:
  - model 进度条 (`█████████░ 60%`)
  - cost 累计
  - rate-limit %
  - subagent list(`progress.jsonl` SubagentStart/Stop derived,粗粒度)

---

## 4. 不做(V0.3.1 deferred → V0.4)

- Claude Code subagent **live progress**(token counts in flight)— 无 API
- 多 harness 同时跑(单 ccteam project = 1 Claude Code session,V0.4 评估)
- harness snapshot 历史 archive(retro 用)— V0.4
- statusline 数据进入 orchestrator 决策(违反 §三 SoT)— 永久 deferred

---

## 5. Open questions(写 V0.3.1 PRD 前问用户)

1. `ClaudeCodeAdapter` 安装入口:`cct doctor --install-statusline-adapter`
   还是 `make setup` 自动包?后者更友好,前者更 explicit。
2. 用户原 `statusline-command.sh` 已自定义(本机就是)— wrapper 策略:
   tee + 透传 vs 完全替换 vs 用户手 source 我们的 helper?
3. `~/.ccteam/harness/<slug>.json` 文件 lifecycle:每条 stdin JSON 完
   全覆盖,还是 append delta 历史?覆盖更简单,delta 历史方便 retro。
4. `meta-agent` session(`<handle>-meta` 路径)是否也要 ship harness panel?
   还是 dashboard 只显示 project session?
5. CodexAdapter 落 V0.3.1 stub 还是 V0.3.2?用户那份 codex-integration
   research doc 里 stdin JSON 形态确认了吗?

---

## 6. 参考

- `docs/research/ccteam-codex-integration.md`(用户 push,2026-05-10)— Codex 适配的前置研究
- `docs/research/thin-harness-fat-skills-architecture-improvement.md` — harness 抽象的更广讨论
- `~/.claude/statusline-command.sh` — 当前 stdin JSON 字段参考
- V0.3 PRD §10 / V0.3 dev-plan §6 — V0.3 ship 边界,V0.3.1 起点
- V0.2.2 F39 marker 模式(`<!-- ccteam-managed:skill -->`)— statusline wrapper 借鉴

---

## 7. 时间线

- 2026-05-10 14:28(UTC+0):用户 Telegram message_id 311 — "先临时记录到一个文件"
- M5.4 ship gate 完事 → V0.3.0 落地
- 之后正式起 `docs/v0-3-1/` patch 文档,本文件可归档(留 git history)或并入 PRD §1 背景
