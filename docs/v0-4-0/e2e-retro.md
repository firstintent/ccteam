# V0.4.0 e2e Retro

> F69 ship gate 验收记录。本文档由主 session 在 ship 前补充。
>
> 当前状态：**skeleton（占位）** — F69 docs prep PR 创建了本文件骨架，
> 待 F60-F68 全部 merge 后，主 session 跑完整 ship gate 验收命令并填入
> 实际输出。

---

## 1. Ship gate 命令执行结果

- [ ] `cargo test --workspace` baseline: TBD（期望 ≥ 866，failed = 0）
- [ ] `cargo clippy --workspace --all-targets`: TBD（期望 ≤ 9，pre-existing baseline）
- [ ] `cargo fmt -- --check $(git diff --name-only origin/main..HEAD | grep '\.rs$')`: TBD
- [ ] `cargo build --workspace --release`: TBD（期望 0 errors）
- [ ] 完整红线 grep 矩阵（[`dev-plan.md`](dev-plan.md) §12）: TBD

### 1.1 红线 grep 矩阵实际输出

```
TBD — 主 session 在 ship gate 时贴 §12 所有命令的实际输出
```

---

## 2. 发现的问题 + 修复记录

> F60-F68 ship 过程中发现的需要 cross-cutting 修复的问题，集中在这里记录。

TBD

---

## 3. 手动 smoke test 结果

### 3.1 Workflow 启动

```bash
ccteam run smoke-test 2>&1 | head -20
```

期望：`workflow_start` event 出现，不 panic。

实际：TBD

### 3.2 Web UI

```bash
ccteam web --bind 127.0.0.1:7331 &
curl -s http://127.0.0.1:7331/health | python3 -m json.tool
curl -s http://127.0.0.1:7331/api/v1/projects | python3 -m json.tool | head -20
```

期望：health OK，projects 返回 JSON 列表（含 `workflow_summary` 字段）。

实际：TBD

### 3.3 Manual trigger smoke

```bash
ccteam ctl spawn-agent --slug smoke-test --role worker
ccteam ctl observe --slug smoke-test
```

期望：session 出现，state.json 可读。

实际：TBD

### 3.4 Artifact trigger smoke

```bash
# 1. 启动 workflow 后写 artifact 文件
echo "test" > .ccteam/issues/smoke-001.md

# 2. 检查下游 agent 是否被自动 spawn
sleep 3
ccteam ctl observe --slug smoke-test
```

期望：fixer 自动 spawn。

实际：TBD

### 3.5 Gate 解锁 smoke

```bash
ccteam ctl trigger-gate --slug smoke-test --gate shipper
sleep 2
ccteam ctl observe --slug smoke-test
```

期望：shipper session 启动。

实际：TBD

---

## 4. Playwright e2e 前端测试

```bash
cd crates/ccteam-web/web && npm run test 2>&1 | tail -10
```

期望：全绿（或 skip 说明原因）。

实际：TBD

---

## 5. V0.4.1 candidates / Known issues

> ship 过程中识别但本轮不修的事项，记录在这里供下个 patch round 处理。

TBD

---

## 6. Ship 总结

- workspace version bump：`0.3.2` → `0.4.0` — TBD
- CLAUDE.md baseline 更新：TBD
- 文档清单：
  - [ ] `docs/v0-4-0/prd.md` — locked（F69 前已 ship）
  - [ ] `docs/v0-4-0/dev-plan.md` — locked（F69 前已 ship）
  - [ ] `docs/v0-4-0/README.md` — locked（F69 前已 ship）
  - [ ] `docs/v0-4-0/user-manual.md` — F69 docs prep 写完
  - [ ] `docs/v0-4-0/migration-guide.md` — F69 docs prep 写完
  - [ ] `docs/v0-4-0/e2e-retro.md` — 本文件（ship 时填）
  - [ ] `examples/workflows/*` — F69 docs prep 写完
  - [ ] `CLAUDE.md §一` — ship 时回填新 baseline + version
  - [ ] `docs/dev-coupling-audit.md` — F60-F69 entry 全量
  - [ ] `docs/interfaces.md` — workflow.yaml schema + 17 个 MCP 工具签名
- ship 决策：TBD（go / no-go）

---

## 7. 后续 patch round 起点

V0.4.0 ship 后，下一个 patch round 候选（V0.4.1）：

- TBD（按本次 e2e 中发现的问题列）

---

> **本文件待补充**。F69 docs prep PR 只占位，
> 主 session 在 ship gate 时按上述模板填入实际命令输出 + 决策记录。
