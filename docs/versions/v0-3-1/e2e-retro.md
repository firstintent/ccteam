# V0.3.1 E2E Retro

> 范围:V0.3.1 F46-F51 ship gate 端到端验证。
>
> base = `origin/main` `f9baf3f`(V0.3 ship 终点);目标版本 `0.3.1`。
>
> 最终测试 baseline: **833/0**(`cargo test --workspace`,F51 ship gate 实测)。
>
> 方法:**4 suite 验证矩阵**,各自隔离 env。本文只落档结果,不夹带代码修复。

---

## 1. 测试方法

### 隔离 env

每 suite 使用独立 `/tmp/ccteam-v031-e2e-<suite>-<pid>/` 根目录,并重定向
ccteam / Claude / projects 状态,避免污染真实用户环境:

```bash
export E2E_ROOT=/tmp/ccteam-v031-e2e-<suite>-$$
mkdir -p "$E2E_ROOT"/{ccteam-home,claude-home,projects-root,xdg-config}
export CCTEAM_HOME=$E2E_ROOT/ccteam-home
export CLAUDE_CONFIG_HOME=$E2E_ROOT/claude-home
export XDG_CONFIG_HOME=$E2E_ROOT/xdg-config
export CCTEAM_PROJECTS_ROOT=$E2E_ROOT/projects-root
export HOME=$E2E_ROOT
export CCTEAM_AUTO_SLUG=off
```

约束:不烧真实 LLM cost;不读写真实 `~/.ccteam` / `~/.claude`;tmux session 使用
唯一前缀并由 suite 清理;不改 git state。

### Suite 划分

| Suite | 覆盖 | 对应 finding |
|---|---|---|
| **A** flex multi-session | `kind: flex` bootstrap、`session add/ls/attach/rm`、sid / tmux / progress scoping | F48 / F49 |
| **B** harness adapter | `HarnessAdapter` trait、`ClaudeCodeAdapter`、statusline dual-write、SSE harness snapshot、Codex stub wiring | F46 / F47 |
| **C** web UI | kind 列、flex session cards、harness badge、session detail route、SSE by sid filter | F50 |
| **D** codex stub | `harness = codex` schema / CLI、NotImplemented 错误语义、混合 harness 共存、文档 deferred 链接 | F47 / F51 |

---

## 2. Suite-by-Suite 结果

### Suite A — flex multi-session

| # | 场景 | Verdict |
|---|---|---|
| A1 | `ccteam team init scratch --kind flex --author-name "$USER"` 生成空 phase team | PASS |
| A2 | flex 项目保留 hooks / cost / progress / memory bridge | PASS |
| A3 | flex 项目关闭 auto loop / phase inject / golden rules | PASS |
| A4 | `session add` 创建 `sessions/<sid>/` 与 master `state.json::sessions` | PASS |
| A5 | sid 单调递增,删除后不复用 | PASS |
| A6 | tmux 命名 `ccteam-<slug>-<sid>` | PASS |
| A7 | progress 写入 `progress/<slug>/<sid>.jsonl` | PASS |
| A8 | `session rm` 为唯一显式 kill 路径 | PASS |

**Suite A verdict:PASS**

### Suite B — harness adapter

| # | 场景 | Verdict |
|---|---|---|
| B1 | `HarnessAdapter` trait 编译通过且无 team 名字面量 | PASS |
| B2 | `ClaudeCodeAdapter` spawn / attach / ingest 主路径保持兼容 | PASS |
| B3 | statusline dual-write 向旧字段与 harness snapshot 同步 | PASS |
| B4 | web SSE 暴露 harness snapshot stream | PASS |
| B5 | 旧 workflow 项目仍走 flat progress path | PASS |
| B6 | `cargo test --workspace` baseline | PASS(833/0) |

**Suite B verdict:PASS**

### Suite C — web UI

| # | 场景 | Verdict |
|---|---|---|
| C1 | dashboard 显示 team `kind` | PASS |
| C2 | flex 详情页显示 per-session cards | PASS |
| C3 | session card 显示 harness badge / sid / 状态 | PASS |
| C4 | `/session/<slug>/<sid>` 详情 route 可打开 | PASS |
| C5 | SSE filter by sid 不串 session | PASS |
| C6 | 非 flex 项目 UI 兼容 V0.3 单 session | PASS |

**Suite C verdict:PASS**

### Suite D — codex stub

| # | 场景 | Verdict |
|---|---|---|
| D1 | `team.yaml::sessions[].harness` 解析 `claude` / `codex` | PASS |
| D2 | CLI `--harness codex` 返回文档化 stub error | PASS |
| D3 | CodexAdapter spawn / ingest 返回 `NotImplemented` | PASS |
| D4 | 错误 message 指向 V0.3.2 real implementation deferred | PASS |
| D5 | team schema 允许 claude + codex stub 混合声明 | PASS |
| D6 | README deferred 链接到 `docs/research/ccteam-codex-integration.md` | PASS |

**Suite D verdict:PASS**

---

## 3. Findings / Verdict

| 维度 | 数 |
|---|---|
| Ship-blocking issues | 0 |
| P1 follow-up | 0 |
| P2 docs nit | 0 |
| Need-real-claude-smoke | 0 |
| Suites with all-PASS | 4 |

V0.3.1 **可 ship**。F46-F51 主路径已覆盖:Claude harness 抽象不破坏旧项目,
Codex 以 stub 形式完成协议占位,flex 支持用户驱动多 session,web UI 能按 sid
观察 session,ship gate 文档链路闭合。

Codex 真 spawn / ingest / hook surface **不在 V0.3.1 范围**,已 deferred 到
V0.3.2,设计路线见 `docs/research/ccteam-codex-integration.md`。

---

## 4. Numbers

- **Suite 数**:4
- **场景数**:26(A:8 / B:6 / C:6 / D:6)
- **PASS**:26
- **FAIL**:0
- **F-finding**:0
- **Real LLM cost burned**:0
- **真实用户目录污染**:0

---

## Changelog

- 2026-05-10:初版。基于 V0.3.1 F51 ship gate 四 suite 结果落档;最终
  `cargo test --workspace` baseline = 833/0。
