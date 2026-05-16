# V0.4.3 — hotfix: slug grammar validation

> 单 finding hotfix,无独立 prd/dev-plan。Ship 后归档。

## 1 个 finding

- **F76** `validate_slug_format` — `ccteam new` / `ccteam init --slug` 接受的 slug 必须 `[a-z0-9-]+`,长 ≤60,首尾不可 `-`(参考 RFC 1123 hostname 子集);超规直接 fail-loud + 提示规则。collision 报错 wording 也优化("already registered at <path>" 而非 generic "duplicate")。

## 触发

V0.4.2 落地后用户反馈:`ccteam new "做一个 todo cli"` / `ccteam new BadSlug` 等中文/空格/大写 slug 不报错但后续路径生成异常。F76 在 CLI 入口处守红线。

## 与 V0.4.2 / V0.4.4 的关系

- V0.4.2(F72-F75)已 ship — 本 hotfix 不改 `init` 主流程
- V0.4.4(F77)修了 hooks / daemon / MCP 在任意路径项目下的 slug → path 解析(`session_context_from_cwd` walk-up),独立问题

## 详情

代码改 1 个文件 + 测试矩阵 5 个用例,见 commit `2bf1814` (`v0.4.3: F76`)。无 PRD/dev-plan 文档 — 改动 < 100 LOC,纯输入校验。
