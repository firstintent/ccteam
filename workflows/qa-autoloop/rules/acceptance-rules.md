# Acceptance Rules(验收规则)

## 【铁律】验收只在 Staging 环境执行

所有自动化验收**只针对 `test.staging_url`**,严禁对 `test.production_url` 执行自动化验收或写操作。

## 验收时机
在 staging 部署完成、commit SHA 验证通过后由 releaser 执行。

## 验收方式
按 `.ccteam/config.json::frontend_test.kind` 选用对应框架:
- `playwright` / `cypress` → headless 浏览器对 `test.staging_url` 跑定向验收
- `none` → 跳过 UI 验收,只验 SHA 匹配 + HTTP 2xx

(后端 / 纯 API 项目通常 `frontend_test.kind = "none"`,验收阶段仅做 SHA 对齐确认。)

## 各优先级验收标准

具体验收点是**项目特定的**。下面是通用框架,各项目按实际功能填充。

### P1 验收(阻断类)
- 修复涉及的 URL / 端点访问,HTTP 2xx 或 3xx
- 页面 / 响应可渲染,无 5xx
- 关键 happy-path(单点)可走完

### P2 验收(SEO / 标题 / 内容类)
- 各受影响页面 `<title>` 与期望值匹配
- 主要响应 / DOM 结构符合预期

### P3 验收(内容 / dashboard / 数据类)
- 受影响页面有可见内容(body text 长度 > 阈值,或 API 返回非空)

### P4 验收
- 跳过自动验收,人工确认

## 验收通过后操作
1. (可选)在 GitHub PR 添加评论:
   ```
   ✅ Staging 验收通过 (commit <sha[:7]>)
   ccteam releaser agent 自动验证于 <datetime>
   ```
2. 更新 `.ccteam/issues/<issue_id>.json`: `status → "closed"`, `closed_at`
3. 更新 `.ccteam/prs/<pr_number>.json`: `accepted: true`, `deployed: true`
4. 写 `.ccteam/acceptance/run-<pr_number>-<ts>.json` 记录本次验收
5. 若 issue 关联 `scenario_id`,重置对应 `.ccteam/backlog/<scenario_id>.json` 的 `status` 为 `pending`(下一轮 tester 重新验证)
6. 有任何 backlog 场景被 reset → drop marker 到 `.ccteam/triggers/tester/releaser-<ts>.json` 唤醒 tester

## 验收失败后操作
1. (可选)在 GitHub PR 添加失败评论
2. `.ccteam/issues/<issue_id>.json`: `status → "open"`, `fix_attempts += 1`
3. 若 `fix_attempts >= 3` → `status → "needs-human"`
4. drop marker 到 `.ccteam/triggers/fixer/<issue_id>.json` 唤醒 fixer 重试
5. 写 `.ccteam/acceptance/run-<pr_number>-<ts>.json` 记录失败原因
